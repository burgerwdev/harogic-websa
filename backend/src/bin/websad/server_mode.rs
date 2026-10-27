//! SoapySDRServer owns the device exclusively until its child has been reaped.
use serde_json::{json, Value};
use std::{collections::VecDeque, io::{self, Read}, net::IpAddr,
    os::{fd::AsRawFd, unix::process::{CommandExt, ExitStatusExt}}, path::{Path, PathBuf},
    process::{Child, ChildStderr, ChildStdout, Command, Stdio},
    sync::mpsc::{self, Receiver, Sender},
    thread::{self, JoinHandle}, time::{Duration, Instant, SystemTime, UNIX_EPOCH}};

const LOG_LINES: usize = 200;
const LOG_BYTES: usize = 64 * 1024;
const CHUNK_BYTES: usize = 2048;
const READY_TIMEOUT: Duration = Duration::from_secs(8);
const STOP_GRACE: Duration = Duration::from_secs(2);

unsafe extern "C" {
    fn fcntl(fd: i32, command: i32, ...) -> i32;
    fn getpid() -> i32;
    fn getppid() -> i32;
    fn kill(pid: i32, signal: i32) -> i32;
    fn prctl(option: i32, arg2: u64, arg3: u64, arg4: u64, arg5: u64) -> i32;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Run { Stopped, Starting, Listening, Stopping, Failed }
impl Run {
    fn name(self) -> &'static str { match self {
        Self::Stopped=>"stopped", Self::Starting=>"starting", Self::Listening=>"listening",
        Self::Stopping=>"stopping", Self::Failed=>"failed",
    }}
}

struct Output<R> { reader: R, pending: Vec<u8> }
impl<R: Read + AsRawFd> Output<R> {
    fn new(reader: R) -> io::Result<Self> {
        // Linux F_GETFL/F_SETFL/O_NONBLOCK. No reader thread can outlive the child
        // or hold stop hostage when a descendant retains the write end.
        let flags = unsafe { fcntl(reader.as_raw_fd(), 3) };
        if flags < 0 || unsafe { fcntl(reader.as_raw_fd(), 4, flags | 0x800) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { reader, pending: Vec::with_capacity(CHUNK_BYTES) })
    }

    fn drain(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        let mut buffer = [0u8; CHUNK_BYTES];
        // A continuously writing child must not monopolize the state lock.
        for _ in 0..32 {
            match self.reader.read(&mut buffer) {
                Ok(0) => { self.flush(&mut lines); break; }
                Ok(length) => for byte in &buffer[..length] {
                    if *byte == b'\n' { self.flush(&mut lines); }
                    else {
                        self.pending.push(*byte);
                        if self.pending.len() == CHUNK_BYTES { self.flush_chunk(&mut lines); }
                    }
                },
                Err(error) if error.kind()==io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind()==io::ErrorKind::WouldBlock => break,
                Err(error) => { self.flush(&mut lines); lines.push(format!("pipe read failed: {error}")); break; }
            }
        }
        lines
    }

    fn flush_chunk(&mut self, lines: &mut Vec<String>) {
        // A pipe read can split a UTF-8 character. Carry only its incomplete
        // suffix into the next bounded chunk; invalid bytes remain lossy raw text.
        let end = match std::str::from_utf8(&self.pending) {
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            _ => self.pending.len(),
        };
        lines.push(String::from_utf8_lossy(&self.pending[..end]).into_owned());
        self.pending.drain(..end);
    }

    fn flush(&mut self, lines: &mut Vec<String>) {
        if !self.pending.is_empty() {
            if self.pending.last()==Some(&b'\r') { self.pending.pop(); }
            lines.push(String::from_utf8_lossy(&self.pending).into_owned());
            self.pending.clear();
        }
    }
}

struct Owned {
    process: Child,
    stdout: Output<ChildStdout>,
    stderr: Output<ChildStderr>,
    started: Instant,
    ready: bool,
    // Dropped only after this child is reaped. The spawning thread blocks on
    // the other end, so parent death is armed for exactly this child's life.
    lifetime: Sender<()>,
    supervisor: Option<JoinHandle<()>>,
}

pub(super) struct SoapyServer {
    state: Run,
    bind_address: IpAddr,
    port: u16,
    child: Option<Owned>,
    error: Option<String>,
    exit_code: Option<i32>,
    logs: VecDeque<Value>,
    log_bytes: usize,
    sequence: u64,
    driver_available: bool,
    dependency_error: Option<String>,
    graph_mode: String,
}
impl Default for SoapyServer {
    fn default() -> Self { Self {
        state: Run::Stopped, bind_address: "127.0.0.1".parse().unwrap(), port: 55132,
        child: None, error: None, exit_code: None, logs: VecDeque::new(), log_bytes: 0,
        sequence: 0, driver_available: false, dependency_error: None, graph_mode: "std".into(),
    }}
}
impl SoapyServer {
    pub(super) fn snapshot(&mut self) -> Value {
        self.poll();
        json!({"state":self.state.name(), "bind_address":self.bind_address.to_string(),
            "port":self.port, "pid":self.child.as_ref().map(|c|c.process.id()),
            "uptime_seconds":self.child.as_ref().map_or(0,|c|c.started.elapsed().as_secs()),
            "exit_code":self.exit_code, "error":self.error, "logs":self.logs,
            "log_capacity":LOG_LINES, "driver_available":self.driver_available,
            "dependency_error":self.dependency_error, "graph_mode":self.graph_mode})
    }

    pub(super) fn remember_graph(&mut self, mode: &str) {
        if matches!(mode,"std"|"rta"|"sdr") {self.graph_mode=mode.into();}
    }

    pub(super) fn refresh_dependencies(&mut self) {
        let result = dependencies();
        self.driver_available = result.is_ok();
        self.dependency_error = result.err();
    }

    pub(super) fn configure(&mut self, data: &Value) -> Result<bool, String> {
        // Validate both fields before observing or mutating the process.
        let (address, port) = parse_bind(data, self.bind_address, self.port)?;
        self.poll();
        if self.child.is_some() { return Err("stop SoapySDRServer before configuring its bind address".into()); }
        let changed = (self.bind_address,self.port)!=(address,port);
        self.bind_address=address;
        self.port=port;
        if changed {self.log("lifecycle",format!("bind configured {}",self.endpoint()));}
        Ok(changed)
    }

    pub(super) fn command(&mut self, data: &Value) -> Result<bool, (&'static str,String)> {
        let action = data["action"].as_str().unwrap_or_default();
        // Invalid restart parameters must never stop the current child.
        let (address, port) = parse_bind(data,self.bind_address,self.port).map_err(|e|("bad_parameter",e))?;
        if !matches!(action,"start"|"stop"|"restart"|"configure") {
            return Err(("bad_parameter","action must be start, stop, restart, or configure".into()));
        }
        self.poll();
        if action=="configure" {
            if self.child.is_some() {return Err(("server_busy","stop SoapySDRServer before configuring its bind address".into()));}
            return self.configure(data).map_err(|e|("bad_parameter",e));
        }
        if action=="stop" {return self.stop().map_err(|e|("server_error",e));}
        if action=="start" && self.child.is_some() {
            if (address,port)!=(self.bind_address,self.port) {
                return Err(("server_busy","stop SoapySDRServer before changing its bind address".into()));
            }
            return Ok(false);
        }
        if action=="restart" {self.stop().map_err(|e|("server_error",e))?;}
        self.bind_address=address;
        self.port=port;
        self.start().map(|()|true).map_err(|e|("server_error",e))
    }

    fn start(&mut self) -> Result<(),String> {
        self.refresh_dependencies();
        if let Some(reason)=self.dependency_error.clone() {return self.fail(reason);}
        let binary=trusted_binary("WEBSA_SOAPY_SERVER_BIN","SoapySDRServer");
        let mut command=Command::new(binary);
        command.arg(format!("--bind={}",self.endpoint())).stdin(Stdio::null())
            .stdout(Stdio::piped()).stderr(Stdio::piped());
        module_environment(&mut command);
        self.start_command(command)
    }

    fn start_command(&mut self, command: Command) -> Result<(),String> {
        let (ready_tx,ready_rx)=mpsc::sync_channel(0);
        let (lifetime_tx,lifetime_rx)=mpsc::channel();
        let supervisor=match thread::Builder::new().name("soapy-server".into()).spawn(move || {
            supervise_child(command, ready_tx, lifetime_rx);
        }) {Ok(thread)=>thread,Err(error)=>return self.fail(format!("failed to start SoapySDRServer supervisor: {error}"))};
        let spawned=match ready_rx.recv() {
            Ok(spawned)=>spawned,
            Err(_)=>{
                drop(lifetime_tx);
                let _=supervisor.join();
                return self.fail("SoapySDRServer supervisor exited before spawn".into());
            }
        };
        let (mut process,stdout,stderr)=match spawned {
            Ok(spawned)=>spawned,
            Err(error)=>{
                drop(lifetime_tx);
                let _=supervisor.join();
                return self.fail(error);
            }
        };
        let stdout=Output::new(stdout);
        let stderr=Output::new(stderr);
        let (stdout,stderr)=match (stdout,stderr) {
            (Ok(out),Ok(err))=>(out,err),
            (out,err)=>{
                let _=process.kill(); let _=process.wait();
                drop(lifetime_tx);
                let _=supervisor.join();
                return self.fail(format!("failed to capture SoapySDRServer output: {}",out.err().or_else(||err.err()).unwrap()));
            }
        };
        let pid=process.id();
        self.child=Some(Owned {process,stdout,stderr,started:Instant::now(),ready:false,
            lifetime:lifetime_tx,supervisor:Some(supervisor)});
        self.state=Run::Starting; self.error=None; self.exit_code=None;
        self.log("lifecycle",format!("starting SoapySDRServer pid {pid} on {}",self.endpoint()));
        // The owned child's exact readiness line is authoritative; never connect
        // to the port, which could belong to an unrelated process.
        Ok(())
    }

    pub(super) fn poll(&mut self) {
        self.drain();
        let Some(child)=self.child.as_mut() else {return};
        match child.process.try_wait() {
            Ok(Some(status))=>{
                self.finish_child(status);
                if self.state==Run::Stopping {self.state=Run::Stopped;}
                else {
                    self.state=Run::Failed;
                    if self.error.is_none() {self.error=Some(format!("SoapySDRServer exited with {status}"));}
                }
                self.log("lifecycle",format!("SoapySDRServer exited with {status}"));
            },
            Ok(None)=>{
                if child.ready && self.state == Run::Starting {self.state=Run::Listening;}
                else if !child.ready && child.started.elapsed()>=READY_TIMEOUT {
                    let reason=format!("SoapySDRServer did not report listening on {}",self.endpoint());
                    self.error=Some(reason.clone()); self.log("lifecycle",reason);
                    let _=self.stop(); self.state=Run::Failed;
                }
            },
            Err(error)=>{self.error=Some(format!("cannot reap SoapySDRServer: {error}"));self.state=Run::Failed;}
        }
    }

    pub(super) fn stop(&mut self) -> Result<bool,String> {
        if self.child.is_none() {
            let changed = self.state != Run::Stopped;
            self.state = Run::Stopped;
            return Ok(changed);
        }
        self.state=Run::Stopping;
        let pid=self.child.as_ref().unwrap().process.id();
        self.log("lifecycle",format!("stopping SoapySDRServer pid {pid}"));
        unsafe {kill(pid as i32,15);}
        if self.wait_for_exit(STOP_GRACE)? {return Ok(true);}
        self.log("lifecycle","SoapySDRServer did not stop during grace period; sending SIGKILL".into());
        if let Some(child)=self.child.as_mut() {let _=child.process.kill();}
        if self.wait_for_exit(Duration::from_secs(1))? {return Ok(true);}
        // Keep ownership and reject acquisition until a later poll reaps it.
        let reason="SoapySDRServer did not exit after SIGKILL; device remains reserved".to_string();
        self.state=Run::Failed; self.error=Some(reason.clone()); self.log("lifecycle",reason.clone());
        Err(reason)
    }

    fn wait_for_exit(&mut self, duration:Duration)->Result<bool,String> {
        let deadline=Instant::now()+duration;
        loop {
            self.drain();
            let result=self.child.as_mut().map(|c|c.process.try_wait());
            match result {
                None=>return Ok(true),
                Some(Ok(Some(status)))=>{
                    self.finish_child(status);
                    self.state=Run::Stopped;
                    self.log("lifecycle",format!("SoapySDRServer stopped ({status})"));return Ok(true);
                },
                Some(Err(error))=>{
                    let reason=format!("cannot reap SoapySDRServer: {error}");
                    self.state=Run::Failed;self.error=Some(reason.clone());
                    self.log("lifecycle",reason.clone());return Err(reason);
                },
                Some(Ok(None))=>{},
            }
            if Instant::now()>=deadline {return Ok(false);}
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn finish_child(&mut self, status: std::process::ExitStatus) {
        self.drain();
        self.exit_code=status.code().or_else(||status.signal().map(|s|128+s));
        if let Some(mut child)=self.child.take() {
            // Unblock the spawn thread only after wait has reaped this child.
            drop(child.lifetime);
            if let Some(supervisor)=child.supervisor.take() {let _=supervisor.join();}
        }
    }

    fn endpoint(&self)->String {match self.bind_address {
        IpAddr::V4(ip)=>format!("{ip}:{}",self.port), IpAddr::V6(ip)=>format!("[{ip}]:{}",self.port),
    }}

    fn drain(&mut self) {
        let Some(child)=self.child.as_mut() else {return};
        let stdout=child.stdout.drain();let stderr=child.stderr.drain();
        let expected=format!("Server bound to {}",self.endpoint());
        for (source,lines) in [("stdout",stdout),("stderr",stderr)] {
            for line in lines {
                if line.trim()==expected {if let Some(child)=self.child.as_mut(){child.ready=true;}}
                if line.contains("Server socket bind FAIL:") {
                    self.error=Some(format!("SoapySDRServer could not bind {}: {line}",self.endpoint()));
                }
                self.log(source,line);
            }
        }
    }

    fn fail<T>(&mut self,reason:String)->Result<T,String> {
        self.state=Run::Failed;self.error=Some(reason.clone());self.log("lifecycle",reason.clone());Err(reason)
    }

    fn log(&mut self,source:&str,text:String) {
        // Lossy UTF-8 can expand a chunk by at most 3x; count actual stored bytes.
        while self.logs.len()>=LOG_LINES || self.log_bytes+text.len()>LOG_BYTES {
            let Some(line)=self.logs.pop_front() else {break};
            self.log_bytes=self.log_bytes.saturating_sub(line["text"].as_str().map_or(0,str::len));
        }
        self.log_bytes+=text.len(); self.sequence+=1;
        let time=SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        self.logs.push_back(json!({"seq":self.sequence,"time_ms":time,"source":source,"text":text}));
    }
}
impl Drop for SoapyServer {fn drop(&mut self){let _=self.stop();}}

fn supervise_child(
    mut command: Command,
    ready: mpsc::SyncSender<Result<(Child, ChildStdout, ChildStderr), String>>,
    lifetime: Receiver<()>,
) {
    // Linux associates PDEATHSIG with the spawning thread. It must remain
    // alive until the manager reaps the child, even between command tasks.
    let parent = unsafe { getpid() };
    unsafe {
        command.pre_exec(move || {
            if prctl(1, 9, 0, 0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            if getppid() != parent {
                return Err(io::Error::from_raw_os_error(3));
            }
            Ok(())
        });
    }
    let spawned = command.spawn().map_err(|error| format!("failed to start SoapySDRServer: {error}")).map(|mut process| {
        let stdout = process.stdout.take().expect("piped stdout");
        let stderr = process.stderr.take().expect("piped stderr");
        (process, stdout, stderr)
    });
    if let Err(mpsc::SendError(Ok((mut process, _, _)))) = ready.send(spawned) {
        let _ = process.kill();
        let _ = process.wait();
        return;
    }
    // Stay alive until the manager reaps this exact child. A pooled worker
    // must not carry the death signal into its next task.
    let _ = lifetime.recv();
}

pub(super) fn parse_bind(data:&Value,current:IpAddr,current_port:u16)->Result<(IpAddr,u16),String> {
    let address=match data.get("bind_address") {
        None=>current,
        Some(value)=>value.as_str().ok_or("bind_address must be an IP literal")?
            .parse::<IpAddr>().map_err(|_|"bind_address must be an IP literal".to_string())?,
    };
    let port=match data.get("port") {
        None=>current_port,
        Some(value)=>value.as_u64().filter(|p|(1..=65535).contains(p))
            .ok_or("port must be an integer from 1 to 65535")? as u16,
    };
    Ok((address,port))
}
fn trusted_binary(key:&str,default:&str)->std::ffi::OsString {std::env::var_os(key).unwrap_or_else(||default.into())}
fn module_path()->Option<PathBuf> {
    std::env::var_os("WEBSA_SOAPY_MODULE_PATH").map(PathBuf::from).or_else(||
        std::env::var_os("HOME").map(|h|PathBuf::from(h).join(".local/share/harogic-websa/soapy/lib/SoapySDR/modules0.8")))
}
fn module_environment(command:&mut Command) {
    let mut paths=Vec::new();if let Some(path)=module_path(){paths.push(path);}
    if let Some(inherited)=std::env::var_os("SOAPY_SDR_PLUGIN_PATH"){paths.extend(std::env::split_paths(&inherited));}
    if let Ok(paths)=std::env::join_paths(paths){command.env("SOAPY_SDR_PLUGIN_PATH",paths);}
}
fn find_binary(binary:&std::ffi::OsStr)->bool {
    let path=Path::new(binary);
    if path.components().count()>1 {return path.is_file();}
    std::env::var_os("PATH").is_some_and(|paths|std::env::split_paths(&paths).any(|dir|dir.join(path).is_file()))
}
fn dependencies()->Result<(),String> {
    let server=trusted_binary("WEBSA_SOAPY_SERVER_BIN","SoapySDRServer");
    if !find_binary(&server){return Err(format!("SoapySDRServer binary is missing: {}",server.to_string_lossy()));}
    let util=trusted_binary("WEBSA_SOAPY_UTIL_BIN","SoapySDRUtil");
    if !find_binary(&util){return Err(format!("SoapySDRUtil binary is missing: {}",util.to_string_lossy()));}
    let mut command=Command::new(util);
    command.arg("--info").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    module_environment(&mut command);
    let mut process=command.spawn().map_err(|e|format!("cannot run SoapySDRUtil --info: {e}"))?;
    let stdout=process.stdout.take().expect("piped stdout");
    let stderr=process.stderr.take().expect("piped stderr");
    let stdout=Output::new(stdout);
    let stderr=Output::new(stderr);
    let (mut stdout,mut stderr)=match (stdout,stderr) {
        (Ok(stdout),Ok(stderr))=>(stdout,stderr),
        (stdout,stderr)=>{
            let reason=stdout.as_ref().err().or_else(||stderr.as_ref().err()).map(|error|error.to_string())
                .unwrap_or_else(||"unknown pipe error".into());
            drop(stdout);drop(stderr);
            let _=process.kill();let _=process.wait();
            return Err(format!("cannot capture SoapySDRUtil --info: {reason}"));
        }
    };
    let deadline=Instant::now()+Duration::from_secs(5);let mut found=false;let mut diagnostic=String::new();
    loop {
        for line in stdout.drain().into_iter().chain(stderr.drain()) {
            found|=info_has_harogic(&line);
            if diagnostic.len()<4096 {diagnostic.push_str(&line);diagnostic.push('\n');}
        }
        match process.try_wait() {
            Ok(Some(status))=>{
                for line in stdout.drain().into_iter().chain(stderr.drain()){found|=info_has_harogic(&line);}
                if !status.success(){return Err(format!("SoapySDRUtil --info failed ({status}): {diagnostic}"));}
                if found{return Ok(());}
                return Err(format!("official harogic Soapy driver is unavailable; SoapySDRUtil --info did not list it (module path: {})",
                    module_path().map_or_else(||"system defaults".into(),|p|p.display().to_string())));
            },
            Ok(None)=>{},
            Err(error)=>{let _=process.kill();let _=process.wait();return Err(format!("cannot wait for SoapySDRUtil: {error}"));}
        }
        if Instant::now()>=deadline {let _=process.kill();let _=process.wait();return Err("SoapySDRUtil --info timed out".into());}
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn info_has_harogic(text:&str)->bool {
    text.lines().any(|line|line.strip_prefix("Available factories...").is_some_and(|factories|
        factories.split(',').any(|factory|factory.trim()=="harogic")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn bind_validation_is_atomic_and_accepts_both_ip_families() {
        let address="127.0.0.1".parse().unwrap();
        for data in [json!({"port":0}),json!({"port":65536}),json!({"port":1.5}),
            json!({"port":"55132"}),json!({"port":null}),json!({"bind_address":"localhost"}),
            json!({"bind_address":"127.0.0.1:55132"}),json!({"bind_address":false})] {
            assert!(parse_bind(&data,address,55132).is_err(),"{data}");
        }
        let (ip,port)=parse_bind(&json!({"bind_address":"::1","port":1}),address,55132).unwrap();
        assert_eq!(ip,"::1".parse::<IpAddr>().unwrap());assert_eq!(port,1);
        assert_eq!(parse_bind(&json!({"port":5555}),address,55132).unwrap(),(address,5555));
        let mut server=SoapyServer::default();
        assert!(server.configure(&json!({"bind_address":"10.0.0.1","port":0})).is_err());
        assert_eq!(server.bind_address,address);assert_eq!(server.port,55132);
    }

    #[test]
    fn dependency_parser_requires_the_registered_lowercase_factory() {
        assert!(info_has_harogic("Available factories... plutosdr, harogic, remote"));
        assert!(!info_has_harogic("Module found: /tmp/harogic-but-broken.so"));
        assert!(!info_has_harogic("Available factories... HAROGIC, remote"));
    }

    #[test]
    fn oversized_non_newline_output_and_foreign_pipe_writer_are_bounded() {
        use std::os::unix::net::UnixStream;
        use std::io::Write;
        let (reader,mut writer)=UnixStream::pair().unwrap();
        writer.set_nonblocking(true).unwrap();
        let mut output=Output::new(reader).unwrap();let mut server=SoapyServer::default();
        for _ in 0..1000 {
            writer.write_all(&[b'x';CHUNK_BYTES]).unwrap();
            for text in output.drain(){server.log("stdout",text);}
        }
        assert!(server.log_bytes<=LOG_BYTES);assert!(server.logs.len()<=LOG_LINES);
        assert!(output.pending.len()<CHUNK_BYTES);
        // Writer stays alive. Reading never waits for EOF or a newline.
        assert!(output.drain().is_empty());
    }

    #[test]
    fn log_chunks_preserve_a_utf8_character_split_across_reads() {
        use std::os::unix::net::UnixStream;
        use std::io::Write;
        let (reader, mut writer) = UnixStream::pair().unwrap();
        let mut output = Output::new(reader).unwrap();
        let prefix = "x".repeat(CHUNK_BYTES - 1);
        writer.write_all(prefix.as_bytes()).unwrap();
        writer.write_all(&[0xe2]).unwrap();
        let first = output.drain();
        assert_eq!(first, vec![prefix]);
        assert_eq!(output.pending, vec![0xe2]);
        writer.write_all(&[0x82, 0xac, b'\n']).unwrap();
        assert_eq!(output.drain(), vec!["€"]);
    }

    fn owned_python(program:&str,port:u16)->SoapyServer {
        let mut command=Command::new("python3");
        command.args(["-u","-c",program,&port.to_string()])
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut server=SoapyServer::default();server.port=port;
        server.start_command(command).unwrap();
        server
    }

    #[test]
    fn supervised_manager_subprocess_fixture() {
        let Some(pid_file)=std::env::var_os("WEBSA_TEST_SOAPY_PARENT_PID_FILE") else {return};
        // The command worker retires immediately after returning the manager.
        // Only the manager's dedicated spawn thread may control child lifetime.
        let mut server=thread::spawn(||owned_python(
            "import time;print('Server bound to 127.0.0.1:55132',flush=True);time.sleep(60)",55132))
            .join().unwrap();
        settle(&mut server);
        assert_eq!(server.state,Run::Listening);
        let owned=server.child.as_ref().unwrap();
        assert!(!owned.supervisor.as_ref().unwrap().is_finished());
        std::fs::write(pid_file,owned.process.id().to_string()).unwrap();
        if std::env::var_os("WEBSA_TEST_SOAPY_NORMAL_STOP").is_some() {
            assert!(server.stop().unwrap());
            assert!(server.child.is_none());
            for task in std::fs::read_dir("/proc/self/task").unwrap() {
                let path=task.unwrap().path().join("comm");
                assert_ne!(std::fs::read_to_string(path).unwrap_or_default().trim(),"soapy-server");
            }
            return;
        }
        loop {thread::park();}
    }

    fn parent_fixture(pid_file:&Path, normal_stop:bool)->Child {
        let mut command=Command::new(std::env::current_exe().unwrap());
        command.args(["--exact","server_mode::tests::supervised_manager_subprocess_fixture","--test-threads=1"])
            .env("WEBSA_TEST_SOAPY_PARENT_PID_FILE",pid_file)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::inherit());
        if normal_stop {command.env("WEBSA_TEST_SOAPY_NORMAL_STOP","1");}
        command.spawn().unwrap()
    }

    #[test]
    fn killing_manager_parent_kills_its_owned_server() {
        let directory=std::env::temp_dir().join(format!("websa_soapy_parent_death_{}",std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let pid_file=directory.join("child.pid");
        let _=std::fs::remove_file(&pid_file);
        let mut parent=parent_fixture(&pid_file,false);
        let deadline=Instant::now()+Duration::from_secs(5);
        let child_pid=loop {
            if let Ok(pid)=std::fs::read_to_string(&pid_file) {
                if let Ok(pid)=pid.parse::<i32>() {break pid;}
            }
            if Instant::now()>=deadline {
                let _=parent.kill();let _=parent.wait();
                panic!("fixture did not start its managed child");
            }
            thread::sleep(Duration::from_millis(10));
        };
        parent.kill().unwrap();
        parent.wait().unwrap();
        let deadline=Instant::now()+Duration::from_secs(3);
        loop {
            // An orphan zombie has stopped; its adopter owns the final reap.
            let status=std::fs::read_to_string(format!("/proc/{child_pid}/status"));
            if status.as_ref().is_err_and(|e|e.kind()==io::ErrorKind::NotFound)
                || status.as_ref().is_ok_and(|text|text.lines().any(|line|line.starts_with("State:")&&line.contains("Z (zombie)"))) {break;}
            if Instant::now()>=deadline {
                unsafe {kill(child_pid,9);}
                panic!("owned server survived SIGKILL of its manager parent");
            }
            thread::sleep(Duration::from_millis(10));
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn normal_manager_stop_joins_its_spawn_thread() {
        let directory=std::env::temp_dir().join(format!("websa_soapy_normal_stop_{}",std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let mut parent=parent_fixture(&directory.join("child.pid"),true);
        assert!(parent.wait().unwrap().success());
        std::fs::remove_dir_all(directory).unwrap();
    }
    fn settle(server:&mut SoapyServer) {
        let deadline=Instant::now()+Duration::from_secs(3);
        while server.state==Run::Starting && Instant::now()<deadline {server.poll();std::thread::sleep(Duration::from_millis(10));}
    }

    #[test]
    fn foreign_listener_does_not_make_owned_child_ready_and_bind_failure_is_kept() {
        let listener=TcpListener::bind("127.0.0.1:0").unwrap();let port=listener.local_addr().unwrap().port();
        let mut server=owned_python("import socket,sys\ns=socket.socket()\ntry: s.bind(('127.0.0.1',int(sys.argv[1])))\nexcept OSError as e: print('Server socket bind FAIL: '+str(e),file=sys.stderr)",port);
        settle(&mut server);
        assert_eq!(server.state,Run::Failed);assert!(server.child.is_none());
        assert!(server.error.as_deref().unwrap().contains("could not bind"));
        assert!(server.logs.iter().any(|line|line["source"]=="stderr"));
        assert_eq!(server.exit_code,Some(0));
    }

    #[test]
    fn silent_child_times_out_and_is_reaped_without_claiming_a_listener() {
        let mut server=owned_python("import time;time.sleep(60)",55132);
        server.child.as_mut().unwrap().started=Instant::now()-READY_TIMEOUT;
        server.poll();
        assert_eq!(server.state,Run::Failed);
        assert!(server.child.is_none());
        assert!(server.error.as_deref().unwrap().contains("did not report listening"));
        assert!(server.logs.iter().any(|line|line["text"].as_str().unwrap().contains("did not report listening")));
    }

    #[test]
    fn listening_stop_reaps_and_later_child_failure_retains_reason() {
        let listener=TcpListener::bind("127.0.0.1:0").unwrap();let port=listener.local_addr().unwrap().port();drop(listener);
        let mut server=owned_python("import socket,sys,time\ns=socket.socket();s.bind(('127.0.0.1',int(sys.argv[1])));s.listen();print('Server bound to 127.0.0.1:'+sys.argv[1]);time.sleep(60)",port);
        settle(&mut server);assert_eq!(server.state,Run::Listening);
        assert!(server.configure(&json!({"port":1})).is_err());
        assert!(server.stop().unwrap());assert!(server.child.is_none());
        assert_eq!(server.state,Run::Stopped);assert!(server.exit_code.is_some());
        let mut failed=owned_python("import sys;print('fatal driver error',file=sys.stderr);sys.exit(7)",port);
        settle(&mut failed);assert_eq!(failed.state,Run::Failed);assert_eq!(failed.exit_code,Some(7));
        assert!(failed.logs.iter().any(|line|line["text"]=="fatal driver error"));
        assert!(failed.error.as_deref().unwrap().contains("exit status: 7"));

        let mut ignores_term=owned_python("import signal,time;signal.signal(signal.SIGTERM,signal.SIG_IGN);print('Server bound to 127.0.0.1:'+__import__('sys').argv[1]);time.sleep(60)",port);
        settle(&mut ignores_term);assert_eq!(ignores_term.state,Run::Listening);
        assert!(ignores_term.stop().unwrap());assert!(ignores_term.child.is_none());
        assert_eq!(ignores_term.exit_code,Some(137));
        assert!(ignores_term.logs.iter().any(|line|line["text"].as_str().unwrap().contains("sending SIGKILL")));
    }
}
