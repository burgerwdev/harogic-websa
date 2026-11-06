//! Exercise real native PCM + model status over the production WebSockets.
//! NR mode is the only receiver control changed; the initial NR settings are
//! restored even on errors. nr_live_validate OUTDIR [SECONDS_PER_MODE]
use futures_util::StreamExt;
use serde_json::{json,Value};
use std::{fs,io::Write,path::Path,process::Command,time::Instant};
use tokio_tungstenite::{connect_async,tungstenite::Message};
fn curl(path:&str,body:Option<&Value>)->Result<Value,Box<dyn std::error::Error>> {
    let mut cmd=Command::new("curl");cmd.args(["-fsS","--max-time","15"]);
    if let Some(body)=body {cmd.args(["-H","Content-Type: application/json","-d",&body.to_string()]);}
    let out=cmd.arg(format!("http://127.0.0.1:8080{path}")).output()?;
    if !out.status.success() {return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());}
    let result:Value=serde_json::from_slice(&out.stdout)?;
    if result["cmd"]=="ERROR" {return Err(result.to_string().into());}Ok(result)
}
struct Restore(Value);
impl Drop for Restore {fn drop(&mut self) {let _=curl("/api/config",Some(&self.0));}}
#[derive(Default)]
struct Audio {
    identity:Option<(u32,u32)>,seq:u32,next_frame:u64, previous:Option<(u64,f64,u32)>,
    packets:u64,frames:u64,peak:f32,overrange:u64,energy:f64,max_gap_ms:f64,last:Option<Instant>,delay:f64,
}
fn u32_at(b:&[u8],i:usize)->u32 {u32::from_le_bytes(b[i..i+4].try_into().unwrap())}
fn u64_at(b:&[u8],i:usize)->u64 {u64::from_le_bytes(b[i..i+8].try_into().unwrap())}
fn f64_at(b:&[u8],i:usize)->f64 {f64::from_le_bytes(b[i..i+8].try_into().unwrap())}
impl Audio {
    fn packet(&mut self,b:&[u8],file:&mut impl Write)->Result<(),Box<dyn std::error::Error>> {
        if b.len()<96 || &b[..4]!=b"AUD2" {return Err("unexpected PCM protocol".into());}
        let identity=(u32_at(b,32),u32_at(b,36));let seq=u32_at(b,8);let frames=u32_at(b,12);
        let channels=u32_at(b,56);let rate=f64_at(b,16);let source_rate=f64_at(b,72);
        if b.len()!=96+frames as usize*channels as usize*4 || rate!=48000.0 || channels!=1 {return Err("unexpected PCM geometry".into());}
        let first=u64_at(b,64);let index=u64_at(b,40);let num=u64_at(b,80);let den=u64_at(b,88);
        if den==0 || num>=den {return Err("invalid source fraction".into());}
        if seq==0 {
            if frames!=0 {return Err("start contains PCM".into());}
            self.identity=Some(identity);self.seq=0;self.next_frame=first;self.previous=None;return Ok(());
        }
        if self.identity!=Some(identity) || seq!=self.seq+1 || first!=self.next_frame {return Err("audio sequence/sample count discontinuity".into());}
        if let Some((old,frac,count))=self.previous {
            let elapsed=(index as i128-old as i128) as f64+num as f64/den as f64-frac;
            if (elapsed-source_rate*count as f64/rate).abs()>1e-6 {
                return Err(format!("PCM source clock discontinuity: identity={identity:?} seq={seq} frame={first} previous=({old}+{frac},frames={count}) current=({index}+{num}/{den},frames={frames}) source_rate={source_rate} actual_advance={elapsed} expected_advance={}",source_rate*count as f64/rate).into());
            }
        }
        self.previous=Some((index,num as f64/den as f64,frames));self.seq=seq;self.next_frame+=frames as u64;
        self.packets+=1;self.frames+=frames as u64;self.delay=f64_at(b,48);
        if !self.delay.is_finite() {return Err("nonfinite delay".into());}
        let now=Instant::now();if let Some(old)=self.last {self.max_gap_ms=self.max_gap_ms.max(now.duration_since(old).as_secs_f64()*1000.0);}self.last=Some(now);
        for sample in b[96..].chunks_exact(4) {
            let value=f32::from_le_bytes(sample.try_into().unwrap());
            if !value.is_finite() {return Err("nonfinite PCM".into());}
            self.peak=self.peak.max(value.abs());self.overrange+=u64::from(value.abs()>1.0);self.energy+=(value as f64).powi(2);
        }
        file.write_all(&b[96..])?;Ok(())
    }
}
#[tokio::main(flavor="current_thread")]
async fn main()->Result<(),Box<dyn std::error::Error>> {
    let args:Vec<_>=std::env::args().collect();if args.len()<2 {return Err("nr_live_validate OUTDIR [SECONDS_PER_MODE]".into());}
    let root=Path::new(&args[1]);fs::create_dir_all(root)?;
    let seconds=args.get(2).map(|s|s.parse::<u64>()).transpose()?.unwrap_or(45);
    let initial=curl("/api/state",None)?;fs::write(root.join("initial-state.json"),serde_json::to_vec(&initial)?)?;
    if initial["connected"]!=true || initial["mode"]!="sdr" {return Err("live SDR must already be connected".into());}
    let selected=args.get(4).map(|s|s.parse::<u32>()).transpose()?.unwrap_or(1);
    let channel=initial["sdr"]["channels"].as_array().ok_or("missing channel registry")?
        .iter().find(|row|row["channel"].as_u64()==Some(u64::from(selected)))
        .ok_or("selected channel is not advertised by the service")?;
    let restore=json!({"cmd":"SET_SDR_CHANNEL","channel":selected,"nr":channel["nr"],
        "nr_algorithm":channel["nr_algorithm"],"nr_atten_db":channel["nr_atten_db"],"nr_strength":channel["nr_strength"]});
    let _restore=Restore(restore);let mut reports=Vec::new();
    let ids:Vec<&str>=if args.get(3).is_some_and(|v|v=="monitor") {
        vec![channel["nr_algorithm"].as_str().ok_or("missing algorithm")?]
    } else if let Some(id)=args.get(3).filter(|s|["dfn","gtcrn","ul-unas","fastenhancer"].contains(&s.as_str())) {
        vec![id.as_str()]
    } else {vec!["dfn","gtcrn","ul-unas","fastenhancer"]};
    for id in ids {
        let response=curl("/api/config",Some(&json!({"cmd":"SET_SDR_CHANNEL","channel":selected,"nr":true,"nr_algorithm":id})))?;
        fs::write(root.join(format!("{id}-config.json")),serde_json::to_vec(&response)?)?;
        let (mut decode,_)=connect_async(format!("ws://127.0.0.1:8080/ws/decode?channel={selected}")).await?;
        // Listening-only changes retain the decoder and intentionally discard
        // the previous reducer's partial tail. Wait for actual model readiness
        // before subscribing to the new steady PCM stream, so the measurement
        // does not straddle that documented source-position discontinuity.
        let ready=tokio::time::timeout(std::time::Duration::from_secs(10),async {
            loop {
                match decode.next().await.ok_or("decode socket ended before model ready")?? {
                    Message::Binary(b) if b.starts_with(b"SDE2")=>{
                        let envelope:Value=serde_json::from_slice(&b[8..])?;let event=&envelope["event"];
                        if event["type"]=="nr-status" && event["requested"]==id {
                            if event["effective"]!=id || event["state"]!="ready" {return Err(event.to_string().into());}
                            break Ok::<Value,Box<dyn std::error::Error>>(event.clone());
                        }
                    },
                    Message::Close(reason)=>return Err(format!("decode closed before model ready: {reason:?}").into()),_=>{},
                }
            }
        }).await??;
        let (mut audio,_)=connect_async(format!("ws://127.0.0.1:8080/ws/audio?channel={selected}")).await?;
        let mut pcm=std::io::BufWriter::new(fs::File::create(root.join(format!("{id}.f32")))?);
        let deadline=tokio::time::Instant::now()+std::time::Duration::from_secs(seconds);
        let mut counts=Audio::default();let mut statuses=vec![ready];
        loop {tokio::select! {
            _=tokio::time::sleep_until(deadline)=>break,
            message=audio.next()=>match message.ok_or("audio socket ended")?? {
                Message::Binary(b)=>counts.packet(&b,&mut pcm)?,
                Message::Close(reason)=>return Err(format!("audio closed: {reason:?}").into()),_=>{},
            },
            message=decode.next()=>match message.ok_or("decode socket ended")?? {
                Message::Binary(b) if b.starts_with(b"SDE2")=>{
                    let envelope:Value=serde_json::from_slice(&b[8..])?;let event=&envelope["event"];
                    if event["type"]=="nr-status" {statuses.push(event.clone());}
                    if event["type"]=="error" {return Err(event.to_string().into());}
                },
                Message::Close(reason)=>return Err(format!("decode closed: {reason:?}").into()),_=>{},
            },
        }}
        pcm.flush()?;let _=audio.close(None).await;let _=decode.close(None).await;
        let state=curl("/api/state",None)?;
        fs::write(root.join(format!("{id}-state.json")),serde_json::to_vec(&state)?)?;
        if state["sdr"]["receiver_error"]!=Value::Null {return Err(state["sdr"]["receiver_error"].to_string().into());}
        let ready=statuses.iter().filter(|s|s["requested"]==id && s["effective"]==id && s["state"]=="ready" && s["model"].is_string()).count();
        if ready==0 || counts.frames<48000 || counts.peak<=1e-6 {return Err(format!("{id}: missing ready status or PCM").into());}
        let report=json!({"channel":selected,"algorithm":id,"seconds":seconds,"audio_packets":counts.packets,"audio_frames":counts.frames,
            "peak":counts.peak,"samples_above_one":counts.overrange,"rms":(counts.energy/counts.frames as f64).sqrt(),"max_audio_gap_ms":counts.max_gap_ms,
            "total_receiver_delay_ms":counts.delay*1000.0,"ready_statuses":ready,"status":statuses.last(),
            "checks":"finite PCM; sample/frame/sequence/source clock continuity; actual model ready; no receiver error"});
        println!("{report}");reports.push(report);
        fs::write(root.join("validation.json"),serde_json::to_vec_pretty(&reports)?)?;
        if counts.overrange>0 {return Err(format!("{id}: {} PCM samples exceed +/-1",counts.overrange).into());}
    }
    drop(_restore);
    let restored=curl("/api/state",None)?;
    fs::write(root.join("restored-state.json"),serde_json::to_vec(&restored)?)?;
    let restored_channel=restored["sdr"]["channels"].as_array().ok_or("missing restored channel registry")?
        .iter().find(|row|row["channel"].as_u64()==Some(u64::from(selected)))
        .ok_or("selected channel disappeared during validation")?;
    for key in ["freq","mode","if_bw","volume","agc","nr","nr_algorithm","nr_atten_db","nr_strength","squelch"] {
        if restored_channel[key]!=channel[key] {return Err(format!("receiver setting not restored: {key}").into());}
    }
    Ok(())
}
