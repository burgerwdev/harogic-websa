#!/usr/bin/env python3
"""Reproducible offline study. Never posts to or configures the running receiver.

Run --help. Outputs contain private radio recordings and belong outside git.
"""
import argparse
import csv
import hashlib
import html
import json
import platform
import shutil
import subprocess
import sys
import tarfile
import time
from fractions import Fraction
from pathlib import Path

import numpy as np
from omlsa import Config, enhance
from scipy import signal, stats
from scipy.io import wavfile

ROOT = Path(__file__).resolve().parents[3]
FS = 48000
# These are signal-structure candidates, NOT verified transcripts or speaker IDs.
CLIPS = [
    ("strong_38", 38, 46, "较强谐波语音候选"),
    ("strong_55", 55, 61, "较强语音及结束候选"),
    ("weak_04", 4, 12, "弱语音候选；可能夹有停顿"),
    ("weak_26", 26, 33, "弱语音候选；可能夹有停顿"),
    ("weak_82", 82, 90, "弱语音候选；可能夹有停顿"),
    ("weak_118", 118, 125, "弱语音候选；可能夹有停顿"),
    ("pause_16", 16, 20, "噪声主导／停顿候选，未经人工确认"),
    ("pause_108", 108, 114, "噪声主导／停顿候选，未经人工确认"),
]
LEVEL_WINDOWS = [(38, 44), (55.5, 59), (95, 103)]


def dump(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False, allow_nan=False)+"\n")


def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as f:
        for chunk in iter(lambda: f.read(1024*1024), b''):
            h.update(chunk)
    return h.hexdigest()


def extract(source, destination):
    """Allow only bounded regular files with safe relative paths; originals are read only."""
    with tarfile.open(source, 'r:*') as archive:
        members = archive.getmembers()
        if sum(m.size for m in members) > 4*1024**3:
            raise ValueError('archive too large for this in-memory study')
        names = set()
        for item in members:
            rel = Path(item.name)
            if rel.is_absolute() or '..' in rel.parts or item.name in names or not item.isfile():
                raise ValueError(f'unsafe or unsupported archive entry {item.name}')
            names.add(item.name)
            target = destination / rel
            target.parent.mkdir(parents=True, exist_ok=True)
            with archive.extractfile(item) as src, target.open('wb') as dst:
                shutil.copyfileobj(src, dst)


def inspect(args):
    out = args.out
    source_hashes = {str(p): digest(p) for p in (args.iq, args.audio)}
    old = out/'inspection.json'
    if old.exists() and json.loads(old.read_text())['input_sha256'] != source_hashes:
        raise ValueError('output directory belongs to different inputs')
    extract(args.iq, out/'input/iq')
    extract(args.audio, out/'input/user_audio')
    metas = list((out/'input/iq').rglob('*.sigmf-meta'))
    if len(metas) != 1:
        raise ValueError('this experiment needs one IQ segment; collections require separate runs')
    meta_path = metas[0]
    meta = json.loads(meta_path.read_text())
    g, rows = meta['global'], meta['annotations']
    if g['core:datatype'] != 'cf32_le' or len(meta['captures']) != 1:
        raise ValueError('unsupported datatype or multiple captures')
    iq_path = meta_path.with_suffix('.sigmf-data')
    iq = np.fromfile(iq_path, dtype='<c8')
    if iq_path.stat().st_size != len(iq)*8 or not np.all(np.isfinite(iq)):
        raise ValueError('invalid IQ')
    fs, clock = g['core:sample_rate'], rows[0]['websa:clock']
    input_rate = Fraction(str(clock['input_rate_hz']))
    step = input_rate/Fraction(str(fs))
    # JSON rounded rate differs from the exact rational by < 1e-12. Infer an
    # integer decimation ONLY when clocks independently prove every block.
    stride = round(float(step))
    if abs(float(step)-stride) > 1e-8:
        raise ValueError('need exact non-integer clock support')
    offset = 0
    for i, row in enumerate(rows):
        c = row['websa:clock']
        assert row['core:sample_start'] == offset
        assert c['sample_format'] == 'cf32_le' and c['actual_rate_hz'] == fs
        assert c['amplitude_scale'] == 1.0, 'implement metadata scaling before replay'
        assert (c['epoch'], c['channel_id'], c['config_revision']) == (clock['epoch'], clock['channel_id'], clock['config_revision'])
        assert int(c['first_input_index']) == int(clock['first_input_index'])+offset*stride
        assert int(c['first_output_index']) == int(clock['first_output_index'])+offset
        assert int(row['websa:skip_samples']) == 0
        assert int(row['websa:sequence']) == int(rows[0]['websa:sequence'])+i
        offset += row['core:sample_count']
    assert offset == len(iq) == int(g['websa:sample_count'])
    task = json.loads((out/'input/user_audio/task.json').read_text())
    frames = [json.loads(x) for x in (out/'input/user_audio/frames.jsonl').read_text().splitlines()]
    audio_segments, previous, previous_sequence = [], None, None
    for segment in task['task']['tracks'][0]['segments']:
        rate, audio = wavfile.read(out/'input/user_audio'/(segment['segment_id']+'.wav'))
        assert rate == FS and audio.ndim == 1 and audio.dtype == np.dtype('float32')
        assert len(audio) == int(segment['samples']) and np.all(np.isfinite(audio))
        config = segment['source']['config']
        assert config['mode'] == 'usb' and config['if_bw'] == 2700 and config['agc']
        assert config['freq'] == meta['captures'][0]['core:frequency']
        assert segment['source']['baseband_stream'] == {'channel_id':clock['channel_id'],'epoch':clock['epoch'],'source_id':clock['source_id']}
        assert segment['source']['receiver_uid'] == g['websa:receiver_uid']
        start = Fraction(int(segment['first_input_index']))+Fraction(int(segment['fraction_num']),int(segment['fraction_den']))
        audio_segments.append({'file':segment['segment_id']+'.wav','samples':len(audio),
            'duration_seconds':len(audio)/rate,'dtype':str(audio.dtype),'rate':rate,
            'config':config,'clock_start_relative_iq_seconds':float((start-int(clock['first_input_index']))/input_rate),
            'peak':float(np.max(abs(audio))),'float_samples_above_1':int(np.sum(abs(audio)>1))})
        offset = 0
        for frame in (f for f in frames if f['segment_id'] == segment['segment_id']):
            a = frame['audio']
            actual = Fraction(int(a['input_index']))+Fraction(int(a['input_fraction_num']),int(a['input_fraction_den']))
            assert actual == start+Fraction(offset,FS)*input_rate
            assert int(frame['segment_sample_start']) == offset
            if previous is not None:
                assert actual == previous
                assert a['sequence'] == previous_sequence+1
            previous = actual+Fraction(a['frames'],FS)*input_rate
            previous_sequence = a['sequence']
            offset += a['frames']
        assert offset == len(audio)
    info = {'input_sha256':source_hashes,'iq_metadata':g,'iq_path':str(iq_path),
        'meta_path':str(meta_path),'iq_samples':len(iq),'iq_duration_seconds':len(iq)/fs,
        'iq_complex_rms':float(np.sqrt(np.mean(abs(iq.astype(complex))**2))),
        'iq_peak':float(np.max(abs(iq))),'iq_dc':{'i':float(iq.real.mean()),'q':float(iq.imag.mean())},
        'iq_blocks':len(rows),'input_stride':stride,'iq_clock':clock,
        'audio_segments':audio_segments,'audio_frames':len(frames),'gaps_verified_absent':True,
        'absolute_utc_known':False,'scale_policy':'cf32_le amplitude_scale=1; no additional scaling; normalized DSP units, not calibrated antenna dBm',
        'recorded_ref_dbm':None,'recorded_ref_note':'These archives do not contain the hardware Ref; do not borrow live Ref.'}
    dump(out/'inspection.json', info)
    return info


def replay(args, info):
    subprocess.run(['cargo','run','--release','--example','offline_ssb_nr','--',
                    info['iq_path'],info['meta_path'],str(args.out/'native')],cwd=ROOT/'backend',check=True)


def correlation(a, b):
    a, b = a-np.mean(a), b-np.mean(b)
    return float(np.dot(a,b)/np.sqrt(np.dot(a,a)*np.dot(b,b)+1e-30))


def align_recorded(out, info, tracks):
    user_audio = np.concatenate([wavfile.read(out/'input/user_audio'/s['file'])[1] for s in info['audio_segments']])
    frames = [json.loads(x) for x in (out/'input/user_audio/frames.jsonl').read_text().splitlines()]
    a = frames[0]['audio']
    clock_start = Fraction(int(a['input_index']))+Fraction(int(a['input_fraction_num']),int(a['input_fraction_den']))
    relative = float((clock_start-int(info['iq_clock']['first_input_index']))/Fraction(str(a['input_rate_hz'])))
    demod_delay = 192/info['iq_metadata']['core:sample_rate']
    native_delay = info['iq_clock']['delay_seconds']+demod_delay
    user_start = relative + native_delay-a['delay_seconds']
    predicted_offset = -user_start*FS
    # Verify the provenance prediction against multiple independent matching DFN
    # regions. Original model/AGC states predate the IQ excerpt: no identity claim.
    results = []
    index = np.arange(len(user_audio))
    for sec, name in [(10,'dfn10'),(38,'dfn10'),(55,'dfn10'),(70,'dfn20'),(96,'dfn20'),(118,'dfn20')]:
        n = int(sec*FS)
        count, search = 3*FS, 3*480  # search +/- 30 ms around the source-clock prediction
        center = round(n+predicted_offset)
        reference = tracks[name][n:n+count].astype(float)[::3]
        target = user_audio[center-search:center+count+search].astype(float)[::3]
        cross = signal.correlate(target-target.mean(), reference-reference.mean(), mode='valid',method='fft')
        peak = int(np.argmax(cross))
        fractional = 0.0
        if 0 < peak < len(cross)-1:
            fractional = .5*(cross[peak-1]-cross[peak+1])/(cross[peak-1]-2*cross[peak]+cross[peak+1])
        estimated = center-search+3*(peak+fractional)-n
        actual = np.interp(np.arange(count)+n+estimated, index, user_audio)
        expected = tracks[name][n:n+count]
        results.append({'native_start_seconds':sec,'baseline':name,'offset_samples':float(estimated),
            'correlation':correlation(actual,expected),
            'recorded_to_offline_rms_ratio_db':float(20*np.log10(np.linalg.norm(actual)/np.linalg.norm(expected)))})
    # Use the clock prediction, not a fitted arbitrary warp. It is independently
    # checked above; preserve the sub-sample offset from exact rational clocks.
    positions = np.arange(len(tracks['none']))+predicted_offset
    valid = (positions >= 0) & (positions <= len(user_audio)-1)
    aligned = np.full(len(positions), np.nan, dtype=np.float32)
    aligned[valid] = np.interp(positions[valid],index,user_audio)
    np.save(out/'user_audio_aligned.npy', aligned)
    report = {'clock_predicted_offset_samples':predicted_offset,'signal_start_relative_native_seconds':user_start,
        'valid_native_start_seconds':float(np.flatnonzero(valid)[0]/FS),
        'valid_native_end_seconds':float(np.flatnonzero(valid)[-1]/FS),
        'dfn_config_change_native_seconds':info['audio_segments'][1]['clock_start_relative_iq_seconds']-0.030,
        'checks':results,'alignment_policy':'source clocks and declared delays; linear sub-sample interpolation; no time warp',
        'comparison_limit':'different initial filter, AGC, resampler and neural states; user audio is a processed reference, never clean truth'}
    dump(out/'alignment.json',report)
    return aligned, report


def lufs(samples):
    # Use ffmpeg's maintained BS.1770/R128 meter on the SAME speech-candidate
    # samples in every branch. Do not match by whole-file RMS or noise pauses.
    command = ['ffmpeg','-hide_banner','-nostats','-f','f32le','-ar',str(FS),'-ac','1','-i','pipe:0',
               '-af','loudnorm=I=-23:TP=-2:LRA=11:print_format=json','-f','null','-']
    r = subprocess.run(command,input=np.asarray(samples,dtype='<f4').tobytes(),capture_output=True,check=True)
    measured, _ = json.JSONDecoder().raw_decode('{'+r.stderr.decode().split('{')[-1])
    return float(measured['input_i'])


def write_pcm24(path, samples):
    path.parent.mkdir(parents=True,exist_ok=True)
    channels = 1 if samples.ndim == 1 else samples.shape[1]
    subprocess.run(['ffmpeg','-y','-v','error','-f','f32le','-ar',str(FS),'-ac',str(channels),
                    '-i','pipe:0','-c:a','pcm_s24le',str(path)],input=np.asarray(samples,dtype='<f4').tobytes(),check=True)


def analyze(out, tracks, diagnostics):
    import matplotlib
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    raw = np.fromfile(out/'native/pre_agc.f32',dtype='<f4')
    f,t,power = signal.spectrogram(raw[::3],16000,nperseg=512,noverlap=384)
    band = (f >= 270) & (f <= 2700)
    smoothed = signal.medfilt(10*np.log10(power[band].mean(axis=0)+1e-30), 31)
    flatness = np.exp(np.mean(np.log(power[band]+1e-30),axis=0))/np.mean(power[band]+1e-30,axis=0)
    figures = out/'figures';figures.mkdir(exist_ok=True)
    fig,axs=plt.subplots(3,1,figsize=(15,9),sharex=True,layout='constrained')
    axs[0].pcolormesh(t,f,10*np.log10(power+1e-30),vmin=-140,vmax=-105,cmap='magma',shading='auto')
    axs[0].set_ylim(0,3400);axs[0].set_ylabel('Audio Hz');axs[0].set_title('Same IQ, native USB, before AGC / NR (relative DSP units)')
    axs[1].plot(t,smoothed);axs[1].set_ylabel('Band PSD, dB')
    axs[2].plot(t,flatness,lw=.4);axs[2].set_ylabel('Spectral flatness');axs[2].set_xlabel('Seconds from IQ excerpt (includes common demodulator delay)')
    for ax in axs:
        for name,a,b,_ in CLIPS:
            ax.axvspan(a,b,alpha=.08,color='green' if name.startswith('strong') else 'blue' if name.startswith('weak') else 'gray')
    fig.savefig(figures/'overview.png',dpi=150);plt.close(fig)
    # IQ spectrum: avoid labeling a narrow peak as a tone until persistence and
    # modulation have been checked. No notch is applied in the shared chain.
    iq = np.fromfile(out/'input/iq/recording/recording.sigmf-data',dtype='<c8')
    meta=json.loads((out/'inspection.json').read_text())
    fi,psd=signal.welch(iq,meta['iq_metadata']['core:sample_rate'],nperseg=2048,return_onesided=False)
    order=np.argsort(fi)
    fig,ax=plt.subplots(figsize=(12,4),layout='constrained');ax.plot(fi[order],10*np.log10(psd[order]+1e-30))
    ax.set(xlabel='IQ offset Hz',ylabel='PSD dB / Hz, DSP units',title='Complete IQ spectrum (positive USB retained; no narrower passband)')
    ax.axvspan(270,2700,alpha=.1,color='green');fig.savefig(figures/'iq_psd.png',dpi=150);plt.close(fig)
    fq,_,sq=signal.spectrogram(iq,meta['iq_metadata']['core:sample_rate'],nperseg=1024,noverlap=512,return_onesided=False)
    order=np.argsort(fq);fq=fq[order];median_psd=np.median(sq[order],axis=1)
    line_ratio=10*np.log10((median_psd+1e-30)/(signal.medfilt(median_psd,31)+1e-30))
    peaks,_=signal.find_peaks(line_ratio,height=6,distance=3)
    persistent=[{'hz':float(fq[i]),'over_local_median_db':float(line_ratio[i])} for i in peaks if -2700<fq[i]<2700]
    quality={'iq_excess_kurtosis_i':float(stats.kurtosis(iq.real)),
        'iq_excess_kurtosis_q':float(stats.kurtosis(iq.imag)),
        'persistent_tone_candidates':persistent,
        'tone_search':'Time-median 1024-bin spectrogram, >6 dB above 31-bin local spectral median inside +/-2700 Hz. This can miss intermittent/weak tones.',
        'noise_dominant_windows':[{'seconds':[a,b],
            'audio_excess_kurtosis':float(stats.kurtosis(raw[int(a*FS):int(b*FS)]))} for a,b in [(16,20),(108,114)] if b*FS<len(raw)],
        'interpretation':'Broadband noise with speech-like harmonic bursts; no physical noise source attribution or verified speech labels.'}
    dump(out/'quality.json',quality)
    # Frame gain diagnostics measure masking, not recovered speech or output SNR.
    rows=[]
    for name,a,b,label in CLIPS:
        cut=slice(int(a*FS),int(b*FS)); entry={'clip':name,'start':a,'end':b,'candidate_label':label}
        for algorithm,x in tracks.items():
            y=x[cut].astype(float)
            if not np.all(np.isfinite(y)):
                continue
            entry[algorithm+'_rms_dbfs']=float(10*np.log10(np.mean(y*y)+1e-30))
            entry[algorithm+'_peak']=float(abs(y).max())
            if algorithm != 'wiener025':
                reference=tracks['none'][cut].astype(float)
                scale=float(np.dot(reference,y)/max(np.dot(reference,reference),1e-30))
                residual=np.linalg.norm(y-scale*reference)/max(np.linalg.norm(y),1e-30)
                entry[algorithm+'_uniform_gain_fit_db']=float(20*np.log10(max(abs(scale),1e-30)))
                entry[algorithm+'_uniform_fit_residual_db']=float(20*np.log10(max(residual,1e-30)))
        for algorithm,d in diagnostics.items():
            active=(d['time'] >= a)&(d['time'] < b)
            selected=(d['frequency']>=270)&(d['frequency']<=2700)
            gain=d['gain'][active][:,selected]
            floor=10**(-d['config']['attenuation_db']/20)
            entry[algorithm+'_median_gain_db']=float(np.median(20*np.log10(np.maximum(gain,1e-20))))
            entry[algorithm+'_bins_at_floor_fraction']=float(np.mean(gain <= floor*1.001))
        rows.append(entry)
    dump(out/'clip_metrics.json',{'audio_level_domain':'speech-window matched listening gain except the supplemental wiener025 raw output',
        'caution':'RMS and spectral attenuation are diagnostics, not SNR or intelligibility scores', 'clips':rows})
    for clip,start,end,_ in [row for row in CLIPS if row[0] in ['strong_38','weak_04','weak_82','weak_118','pause_16']]:
        choices=['none','dfn13','omlsa_author18','omlsa_ssb10']
        fig,axs=plt.subplots(len(choices),1,figsize=(13,8),sharex=True,sharey=True,layout='constrained')
        for ax,name in zip(axs,choices,strict=True):
            x=tracks[name][int(start*FS):int(end*FS):3]
            ff,tt,pp=signal.spectrogram(x,16000,nperseg=512,noverlap=384)
            ax.pcolormesh(tt+start,ff,10*np.log10(pp+1e-30),vmin=-85,vmax=-35,cmap='magma',shading='auto')
            ax.set_ylim(0,3000);ax.set_ylabel(name+'\nHz')
        axs[-1].set_xlabel('Native audio time, s');fig.savefig(figures/(clip+'_matched.png'),dpi=150);plt.close(fig)
    return rows


def write_report(out, info, alignment, levels, provenance):
    tone=json.loads((out/'quality.json').read_text())
    correlations=[r['correlation'] for r in alignment['checks']]
    text=f'''已完成同一 IQ 的原生 USB/AGC 重放、生产 DFN 基线和 OM-LSA＋IMCRA 离线原型比较。当前没有已确认原文或人工盲听结果，未证明呼号、数字、辅音可懂度优于 DFN；本轮不建议替换生产 NR。

输入为两个 tar 容器，原文件 SHA256 见 inspection.json。SigMF 是 cf32_le，{info['iq_metadata']['core:sample_rate']:.12f} Hz，{info['iq_samples']} 个复样本、{info['iq_duration_seconds']:.9f} 秒。{info['iq_blocks']} 个 IQ 块、{info['audio_frames']} 个音频帧的源时钟和序号连续，amplitude_scale=1；无额外缩放。这是 DSP 归一化单位，不能据此直接报告天线输入 dBm。元数据没有硬件 Ref，不能用当前 Ref 代替录音 Ref。源 UTC 不可靠。

音频为 48 kHz 单声道 IEEE float32 WAV。audio-000000 为 67.02 秒／DFN 10 dB，audio-000001 为 60.35 秒／DFN 20 dB。两段保留相同频率 14.270 MHz、USB 2700 Hz、AGC on、volume 2.0；音频来源是 published_audio，包含后端处理，不含浏览器音量。切分由 NR 衰减配置变化引起，时钟没有断开。

延迟对齐采用精确源时钟与声明群延迟，不用音频文件创建时间。用户音频相对本地原生音频的读取偏移为 {alignment['clock_predicted_offset_samples']:.6f} 个 48 kHz 样本；NR 10→20 dB 转换位于本地音频 {alignment['dfn_config_change_native_seconds']:.9f} 秒。用户音频可对齐到本地 0–{alignment['valid_native_end_seconds']:.6f} 秒，末尾约 1.194 秒没有用户音频覆盖。六个独立窗口波形相关系数 {min(correlations):.6f}–{max(correlations):.6f}，相关峰与时钟预测相符。原始滤波、AGC、神经网络历史不同，所以不宣称逐样本相同；处理结果绝不作为干净真值。

所有基线共用一次 AnalogDemod::new_prechannelized 原生解调和现有新 AGC；静噪统一打开以避免门控混淆，volume 2.0 都在 NR 后应用。生产 DFN 10／13／20 dB 确认实际运行 Dfn，无回退；延迟 1440 样本即 30 ms，已移除，尾部补零仅用于排空状态，然后裁回原长度。Wiener 0.25 另存原始结果；它仍是全谱 25% 分位数标量 floor，不能称作 IMCRA。公共解调群延迟 192 / IQ采样率约 28.312 ms 保留在各条时间轴中。

频谱与质量：宽带噪声贯穿语音带，叠有不同强度、间歇性的谐波语音结构。持久窄线搜索在 ±2700 Hz 中发现 {len(tone['persistent_tone_candidates'])} 个超过局部谱中值 6 dB 的候选；这不排除瞬时干扰或弱窄线。IQ I/Q 超额峰度约 {tone['iq_excess_kurtosis_i']:.2f}/{tone['iq_excess_kurtosis_q']:.2f}，16–20 s 与 108–114 s 音频窗口约 0.86/0.42；该证据不支持首先把信号当作强脉冲主导噪声处理，也不能确定电气噪声的物理来源。8 组代表片段的“较强／弱／停顿”仅由频谱和能量推断，待人工确认。

OM-LSA 使用 16 kHz、512 点 Hamming、128 点步长；同一帧的两级最小值跟踪、IMCRA 语音概率控制的逐频点噪声递推、1.4685 偏差补偿、decision-directed a priori SNR、E1 LSA 增益及几何混合，均依据 Cohen 原始论文和作者公开程序。保留 8×15 帧的最小值子窗。作者 w=1 的三点 Hann 实际为 [0,1,0]，本轮保持此设定，不把它描述为有强平滑作用。噪声从首个非零帧初始化，没把开头假装成纯噪声；最初若干秒不用于主结论。

author18 是作者参数方向的移植（medium、tone on、18 dB 名义 GH0），不是 MATLAB 数值一致性认证；修正了文件 I/O、WOLA 边界与尾部长度，采用 scipy 的 exp1，未用 MATLAB/Octave 做逐帧对照。它保留原有硬语音概率与音调判断，GH0 可低于名义 -18 dB。ssb6／ssb10 改用 270–2700 Hz 做帧统计，关闭自动音调判决和低概率硬置零，以连续后验概率计算，并将实际每个频点的增益限制在 [-6,0]／[-10,0] dB；保持其余 IMCRA 递推相同。

32 ms 窗／8 ms 步长的实现是因果帧估计；完整 WOLA 重建需要 24 ms 重叠历史和最多一帧步长的调度等待，总可用延迟按 32 ms 预算。输出按原时间位置放回，是离线对齐，不是零延迟系统。48→16→48 kHz 多相重采样没有额外时轴平移；占用语音带 270–2700 Hz 的重建相对误差 {provenance['resampling_bridge_270_to_2700_hz_relative_error_db']:.2f} dB，全频误差 {provenance['resampling_bridge_relative_error_db']:.2f} dB（这是重建误差，不是输入 SNR）。全频差异主要涉及 8 kHz 以上分量，不能算作降噪收益。bridge16_control_float.wav 与 listening/full/bridge16.wav 单独保留该桥接对照。

初步判断：author18 的频谱出现更明显的稀疏孔洞与断续增益，可能伤及弱辅音或产生音乐噪声，需要试听验证；不据此直接断言某个字已被吞掉。保守 SSB 版本保持更多背景，大多数语音带时频点仍落在设定增益下限，但较强谐波有差别增益，不能把总 RMS 降低解释为弱信号增强。当前证据支持保留离线候选继续盲听，不支持宣称固定 dB 收益或优于 DFN。

一个可量化的重点：在 118–125 s 弱语音候选区，未匹配响度前的 DFN13 几乎等于原输入统一衰减约 12.987 dB，波形相关约 0.999983，去掉最佳常数增益后残差约为输出的 -44.60 dB。该段主要落在衰减底限，不能把较低音量解释为选择性提取弱语音。此诊断没有把该片段当作纯语音，也没有把波形相似度当作可懂度；详见 clip_metrics.json 以及 weak_118_matched.png。OM-LSA 保守版在此段存在更明显的频谱重分配，但其文字辨认收益仍待盲听。

响度匹配使用 ffmpeg BS.1770/R128 表，在同一组语音候选窗 38–44、55.5–59、95–103 s 上测量，各整条轨道只用一个固定增益；不是逐停顿归一化。匹配结果 {min(levels['matched_lufs'].values()):.2f}–{max(levels['matched_lufs'].values()):.2f} LUFS，最大样本峰值 {levels['peak_max']:.6f}，PCM24 试听无样本削波。原始浮点母版有超过 ±1 的峰值，这是整数转换／播放链的潜在削波点，不足以断言采集已经削波。浮点母版完整保留，试听版已统一留余量。

请从 review.html 开始：各片段 A/B/C/D 独立打乱，可在相同播放位置切换。另有 24 组左右声道配对 WAV，左／右身份在文件名中。先独立写呼号／数字，再记辅音、吞字、杂音与偏好；listening_scores.csv 是空白记录表。完成后看 blind_key.json。没有确认原文，不计算抄收正确率；没有干净参考，不对用户 DFN 音频计算 SNR、SI-SDR、PESQ 或 STOI。频谱、RMS、静音变安静只作诊断。

现代电台公开做法与对本项目的启示：
KENWOOD TS-890S 的官方页面将语音 NR1 频谱减法、脉冲 NB1/NB2、IF notch、AF beat cancel 和 AGC 脉冲后恢复分别处理。可借鉴按干扰类型选处理器、避免在无脉冲证据时叠加 blanker；其公开说明不足以认定内部使用 OM-LSA/IMCRA。
https://www2.jvckenwood.com/usa/com/amateur/ts-890s/nr/
Icom IC-7300 官方手册将 NR 定义为随机噪声抑制，并要求把强度调到不使接收信号失真；notch/NB 是独立功能。可借鉴保守默认与可立即旁路的同响度比较。手册未公开完整 NR 内核，不能声称是某个统计或神经算法。
https://icomuk.co.uk/files/icom/PDF/advancedManuals/IC-7300_Full_English%20v6.pdf
FlexRadio 较新的官方 DSP Controls Panel 文档列出传统 NR、LMS NRL、逐频点估计的 NRF、Speex NRS、神经 RNN，以及独立 notch/NB；NRF 提供 512–8192 点窗口并明确频率分辨率与延迟的取舍。可借鉴多算法并存与明确窗口延迟，不必预设神经算法或统计算法必然适合所有弱 SSB。该较新文档优先于 2025 年把所有过滤器描述为无可调参数的 quick reference。
https://helpdesk.flexradio.com/hc/en-us/articles/56306070856475-How-to-Use-the-DSP-Controls-Panel

下一步优先：对 4–12、26–33、82–90、118–125 s 进行上述盲听；若能确认呼号／数字则锁定文本再统计。保守 OM-LSA 若只保留更多底噪、没有改善字词辨认，就记录为无优势。若主要问题是起音／尾辅音断续，下一轮针对语音概率滞回、decision-directed 时间常数与 AGC 增益变化对噪声估计的影响做小范围消融；若确认是窄线或脉冲，再独立试 notch/blanker。继续保持相同 IQ、通带、解调和 AGC，不用现场调 Ref 来掩盖算法差异。

实现与验证可复现步骤见仓库 tools/bench/ssb_nr/README.txt。数值测试覆盖首尾重建、无未来泄漏、逐频点有色噪声／噪声上升跟踪、静音稳定；实录重放验证计数、来源哈希、DFN 非回退、响度误差。当前未做人工听辨或公开 MATLAB 的数值一致性验证。作者公开 omlsa.m 标有 all rights reserved，没有授予生产集成许可；候选仅限本轮离线研究，生产化前另行核对实现授权。

原始资料（已另存 reference/）：
https://israelcohen.com/software/
https://israelcohen.com/wp-content/uploads/2020/11/omlsa.zip
https://israelcohen.com/wp-content/uploads/2018/05/sp_Nov2001.pdf
https://israelcohen.com/wp-content/uploads/2018/05/SAP_Sep2003.pdf
'''
    (out/'report.txt').write_text(text)


def package(out, tracks, recorded):
    candidates={k:v for k,v in tracks.items() if k != 'wiener025'}
    candidates['user_dfn_reference']=recorded
    loudness={}
    for name,x in candidates.items():
        speech=np.concatenate([x[int(a*FS):int(b*FS)] for a,b in LEVEL_WINDOWS])
        if not np.all(np.isfinite(speech)):
            raise ValueError('level window outside common recording')
        loudness[name]=lufs(speech)
    gains={name:10**((-23-level)/20) for name,level in loudness.items()}
    peak=max(float(np.nanmax(abs(candidates[name])))*gain for name,gain in gains.items())
    common=min(1,10**(-2/20)/peak)
    matched={name:np.asarray(x*gains[name]*common,dtype=np.float32) for name,x in candidates.items()}
    for name,x in matched.items():
        # Float mother files preserve the unmodified processing result; PCM24
        # listening files use the common safe peak margin below.
        finite=np.isfinite(x)
        if not np.all(finite):
            # Preserve the actual overlap only; do not invent a silent tail.
            assert np.array_equal(np.flatnonzero(finite),np.arange(np.sum(finite)))
        wavfile.write(out/(name+'_float.wav'),FS,candidates[name][finite].astype(np.float32))
        write_pcm24(out/'listening/full'/(name+'.wav'),x[finite])
    verify_levels={}
    for name,x in matched.items():
        speech=np.concatenate([x[int(a*FS):int(b*FS)] for a,b in LEVEL_WINDOWS])
        verify_levels[name]=lufs(speech)
    if max(verify_levels.values())-min(verify_levels.values()) > .12:
        raise ValueError('listening loudness mismatch > 0.12 LU')
    rng=np.random.default_rng(20261103)
    keys={};items=[]
    with (out/'listening_scores_template.csv').open('w') as f:
        writer=csv.writer(f);writer.writerow(['clip','option','heard_callsign_digits','consonants_1_to_5','syllable_loss_1_to_5','artifacts_1_to_5','preference','notes'])
        for name,a,b,label in CLIPS:
            cut=slice(int(a*FS),int(b*FS))
            choices=['none','dfn13','omlsa_author18','omlsa_ssb10']
            rng.shuffle(choices)
            keys[name]={}
            clip_dir=out/'listening/clips'/name
            if clip_dir.exists():
                for stale in clip_dir.glob('*.wav'):
                    if stale.stem not in ['A','B','C','D']:
                        stale.unlink()
            section=[f'<section><h2>{html.escape(name)} · {a}–{b} s</h2><p>{html.escape(label)}</p>']
            for i,algorithm in enumerate(choices):
                tag=chr(ord('A')+i);keys[name][tag]=algorithm
                x=matched[algorithm][cut]
                if not np.all(np.isfinite(x)):
                    continue
                path=out/'listening/clips'/name/(tag+'.wav')
                write_pcm24(path,x)
                section.append(f'<label>{tag} <audio preload="none" controls src="listening/clips/{name}/{tag}.wav"></audio></label>')
                writer.writerow([name,tag,'','','','','',''])
            section.append('<p>先独立抄写呼号／数字，再比较吞字与杂音；没有确认原文时不要计算正确率。</p></section>')
            items.append('\n'.join(section))
            for left,right in [('none','dfn13'),('none','omlsa_ssb10'),('dfn13','omlsa_ssb10')]:
                pair=np.stack([matched[left][cut],matched[right][cut]],axis=1)
                write_pcm24(out/'listening/stereo_pairs'/(name+'_'+left+'_L_'+right+'_R.wav'),pair)
    dump(out/'blind_key.json',keys)
    if not (out/'listening_scores.csv').exists():
        shutil.copyfile(out/'listening_scores_template.csv',out/'listening_scores.csv')
    levels={'method':'ffmpeg BS.1770/R128 integrated loudness on identical concatenated speech-candidate windows; one constant gain per whole track',
        'windows_seconds':LEVEL_WINDOWS,'unmatched_lufs':loudness,'per_track_gain_db':{k:20*np.log10(v) for k,v in gains.items()},
        'common_peak_margin_db':20*np.log10(common),'matched_lufs':verify_levels,
        'peak_max':float(max(np.nanmax(abs(x)) for x in matched.values())),
        'note':'Windows inferred from signal structure, not clean speech. Weak clips deliberately retain relative syllable attenuation; no per-pause normalization.'}
    dump(out/'loudness.json',levels)
    page='''<!doctype html><html lang="zh"><meta charset="utf-8"><meta name="viewport" content="width=device-width"><title>弱 SSB 离线试听</title><style>
body{font:17px system-ui;max-width:1000px;margin:2em auto;padding:0 1em;color:#17212a;background:#f7fafb}section{background:white;padding:1em;margin:1em 0;border:1px solid #ccd6df;border-radius:8px}h1{font-size:26px}h2{font-size:20px}label{display:flex;align-items:center;gap:1em;margin:.6em}audio{width:90%}img{max-width:100%}a{color:#185c88}</style>
<h1>同一 IQ 的弱 SSB 降噪比较</h1><p>32 ms OM-LSA/IMCRA 原型；USB 2700 Hz、原生 AGC 相同。已校正 NR 延迟，按共同语音候选窗口匹配响度。字母顺序在各片段独立打乱。切换播放器会暂停原音频并保持同一播放位置。</p>
<p>“较强／弱／停顿”是频谱与能量推断，尚待人工确认。用户 DFN 录音只是一项处理结果参考。没有干净参考／确认原文，不报告 SNR、SI-SDR 或抄收正确率。</p>
<p><a href="report.txt">研究说明与结论</a> · <a href="listening_scores.csv">填写试听记录</a> · <a href="blind_key.json">完成后查看算法映射</a></p>
<img src="figures/overview.png" alt="全段解调频谱及片段标记">'''+ '\n'.join(items)+'''
<script>for(const s of document.querySelectorAll('section')){let previous=null;for(const a of s.querySelectorAll('audio'))a.addEventListener('play',()=>{if(previous&&previous!==a){const t=previous.currentTime;previous.pause();a.currentTime=Math.min(t,a.duration||Infinity)}previous=a})}</script></html>'''
    (out/'review.html').write_text(page)
    return matched,levels


def main():
    global CLIPS, LEVEL_WINDOWS
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--iq',type=Path,required=True,help='SigMF tar archive')
    parser.add_argument('--audio',type=Path,required=True,help='published-audio tar archive')
    parser.add_argument('--out',type=Path,required=True,help='independent experiment directory')
    parser.add_argument('--skip-native',action='store_true',help='reuse existing native/ outputs')
    parser.add_argument('--skip-omlsa',action='store_true',help='reuse existing OM-LSA outputs and diagnostics')
    parser.add_argument('--clip-plan',type=Path,help='JSON with clips [name,start,end,label] and level_windows [start,end]; defaults target the supplied 127 s recording')
    args=parser.parse_args();args.out=args.out.resolve();args.out.mkdir(parents=True,exist_ok=True)
    if args.clip_plan:
        plan=json.loads(args.clip_plan.read_text());CLIPS=plan['clips'];LEVEL_WINDOWS=plan['level_windows']
    dump(args.out/'clip_plan.json',{'clips':CLIPS,'level_windows':LEVEL_WINDOWS})
    info=inspect(args)
    if not args.skip_native:
        replay(args,info)
    native=json.loads((args.out/'native/native.json').read_text())
    assert native['input_sha256'] == digest(Path(info['iq_path'])), 'stale native IQ replay'
    assert native['metadata_sha256'] == digest(Path(info['meta_path'])), 'stale native metadata'
    tracks={name:np.fromfile(args.out/'native'/(name+'.f32'),dtype='<f4') for name in ['none','dfn10','dfn13','dfn20','wiener025']}
    assert all(len(x)==native['output_samples'] and np.all(np.isfinite(x)) for x in tracks.values())
    diagnostics={};timings={}
    if (args.out/'omlsa_timings.json').exists():
        timings=json.loads((args.out/'omlsa_timings.json').read_text())
    duration=native['output_samples']/FS
    for a,b in LEVEL_WINDOWS+[(row[1],row[2]) for row in CLIPS]:
        if not 0 <= a < b <= duration:
            raise ValueError('clip outside recording; provide --clip-plan')
    # OM-LSA uses a 48->16->48 kHz bridge. Native DFN stays at 48 kHz.
    # Check both the full band (includes linear-resampler images) and the
    # occupied speech band; do not call the bridge globally transparent.
    samples=np.fromfile(args.out/'native/nr_input.f32',dtype='<f4')
    down=signal.resample_poly(samples.astype(float),1,3)
    bridge=signal.resample_poly(down,3,1)[:len(samples)]
    bridge_error=20*np.log10(np.linalg.norm(bridge[FS:-FS]-samples[FS:-FS])/np.linalg.norm(samples[FS:-FS]))
    bandpass=signal.butter(6,[270,2700],btype='bandpass',fs=FS,output='sos')
    bridge_band_error=20*np.log10(np.linalg.norm(signal.sosfilt(bandpass,bridge-samples)[FS:-FS])/np.linalg.norm(signal.sosfilt(bandpass,samples)[FS:-FS]))
    (bridge*2).astype('<f4').tofile(args.out/'bridge16_control.f32')
    wavfile.write(args.out/'bridge16_control_float.wav',FS,(bridge*2).astype(np.float32))
    tracks['bridge16']=np.asarray(bridge*2,dtype=np.float32)
    nr_input_hash=digest(args.out/'native/nr_input.f32')
    omlsa_source_hash=digest(Path(__file__).with_name('omlsa.py'))
    for name,config in [('omlsa_author18',Config(profile='author',attenuation_db=18)),
                        ('omlsa_ssb6',Config(attenuation_db=6)),('omlsa_ssb10',Config(attenuation_db=10))]:
        path=args.out/(name+'.f32');diag=args.out/(name+'.npz')
        if args.skip_omlsa:
            data=dict(np.load(diag));data['config']=json.loads(str(data['config_json']))
            assert str(data['input_sha256']) == nr_input_hash, 'stale OM-LSA input'
            assert str(data['source_sha256']) == omlsa_source_hash, 'stale OM-LSA implementation'
            assert data['config'] == config.__dict__, 'stale OM-LSA configuration'
            diagnostics[name]=data
        else:
            start=time.monotonic();processed,data=enhance(down,config)
            result=signal.resample_poly(processed,3,1)[:len(samples)]*2
            assert len(result)==len(samples) and np.all(np.isfinite(result))
            result.astype('<f4').tofile(path)
            np.savez_compressed(diag,**{k:v for k,v in data.items() if k!='config'},config_json=json.dumps(data['config']),
                input_sha256=nr_input_hash,source_sha256=omlsa_source_hash)
            diagnostics[name]=data;timings[name]=time.monotonic()-start
            dump(args.out/'omlsa_timings.json',timings)
        tracks[name]=np.fromfile(path,dtype='<f4')
        print(name,'complete',flush=True)
    recorded,alignment=align_recorded(args.out,info,tracks)
    matched,levels=package(args.out,tracks,recorded)
    # Keep Wiener as an extra auditable baseline in metrics and raw files; no
    # claim that its scalar quantile floor implements IMCRA.
    plot_tracks={**tracks,**{k:v for k,v in matched.items() if k!='user_dfn_reference'}}
    analyze(args.out,plot_tracks,diagnostics)
    provenance={'python':sys.version,'platform':platform.platform(),'numpy':np.__version__,
        'git_head':subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip(),
        'git_status':subprocess.check_output(['git','status','--short'],cwd=ROOT,text=True),
        'source_hashes':{str(p.relative_to(ROOT)):digest(p) for p in [ROOT/'backend/examples/offline_ssb_nr.rs',Path(__file__),Path(__file__).with_name('omlsa.py'),ROOT/'backend/src/nr.rs',ROOT/'dsp/core/src/analog/mod.rs',ROOT/'dsp/core/src/analog/agc.rs',ROOT/'frontend/public/models/dfn/DeepFilterNet3_onnx.tar.gz']},
        'seconds':timings,'resampling_bridge_relative_error_db':float(bridge_error),
        'resampling_bridge_270_to_2700_hz_relative_error_db':float(bridge_band_error),
        'intelligibility_verified':False,'ground_truth_transcript':None,
        'notes':['No API writes, no hardware/control changes.','No denoised reference treated as clean.','No SNR/SI-SDR/PESQ/STOI from user DFN audio.']}
    dump(args.out/'provenance.json',provenance)
    write_report(args.out,info,alignment,levels,provenance)
    print('Review:',args.out/'review.html',flush=True)


if __name__=='__main__':
    main()
