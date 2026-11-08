#!/usr/bin/env python3
"""Offline release/frequency-smoothing ablations prompted by echo-like coloration.

Use the previous native study and speech-focus-v1; no live service requests.
Run with OPENBLAS_NUM_THREADS=1 OMP_NUM_THREADS=1 nice -n 10.
"""
import argparse
import csv
import datetime
import html
import json
import shutil
import subprocess
import sys
import time
import wave
from dataclasses import asdict
from pathlib import Path

import numpy as np
from run_speech_focus import CLIPS
from scipy.io import wavfile
from speech_focus import FocusConfig, enhance, recover_components
from study import digest, dump, lufs, write_pcm24

FS=48000


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--study',type=Path,required=True)
    parser.add_argument('--previous',type=Path,required=True)
    parser.add_argument('--out',type=Path,required=True)
    args=parser.parse_args();base=args.study.resolve();prior=args.previous.resolve();out=args.out.resolve()
    if out.exists():
        raise ValueError('choose a fresh directory to preserve previous audio and annotations')
    old=json.loads((prior/'manifest.json').read_text())
    inputs={name:base/'native'/(name+'.f32') for name in ['none','dfn10','dfn13','dfn20','nr_input']}
    for name,path in inputs.items():
        assert digest(path)==old['input_sha256'][name],name
    x,y10,y13,y20=[np.fromfile(inputs[k],dtype='<f4').astype(float) for k in ['none','dfn10','dfn13','dfn20']]
    X,E=recover_components(y10,y20)
    a=10**(-13/20);mix_error=20*np.log10(np.linalg.norm((1-a)*E+a*X-y13)/np.linalg.norm(y13))
    input_error=20*np.log10(np.linalg.norm(X-x)/np.linalg.norm(x))
    assert mix_error < -100 and input_error < -100
    evidence=Path(old['statistical_evidence_file'])
    assert digest(evidence)==old['statistical_evidence_sha256']
    with np.load(evidence) as z:
        probability,times,hz=z['speech_probability'],z['time'],z['frequency']
    configs={
        'guard_v1':FocusConfig(),
        'guard_release24':FocusConfig(release_ms=24),
        'guard_smooth60':FocusConfig(gain_smoothing_hz=60),
        'guard_r24_s60':FocusConfig(release_ms=24,gain_smoothing_hz=60),
    }
    out.mkdir(parents=True);tracks={'dfn20':y20};diagnostics={};timings={}
    for name,cfg in configs.items():
        started=time.monotonic();tracks[name],data=enhance(X,E,probability,times,hz,cfg)
        timings[name]=time.monotonic()-started;diagnostics[name]=data
        tracks[name].astype('<f4').tofile(out/(name+'.f32'))
        np.savez_compressed(out/(name+'_diagnostics.npz'),frame_summary=data['frame_summary'],columns=data['columns'])
        print(name,'finished',round(timings[name],2),'s',flush=True)
    _,golden=wavfile.read(prior/'float/speech_guard.wav')
    assert np.array_equal(tracks['guard_v1'].astype(np.float32),golden), 'default behavior changed from v1'
    windows=old['loudness']['common_speech_windows']
    measured_raw={k:lufs(np.concatenate([x[int(a*FS):int(b*FS)] for a,b in windows])) for k,x in tracks.items()}
    factors={k:10**((-23-v)/20) for k,v in measured_raw.items()}
    peak=max(np.max(abs(x))*factors[k] for k,x in tracks.items());common=min(1,10**(-2/20)/peak)
    matched={k:(x*factors[k]*common).astype(np.float32) for k,x in tracks.items()}
    levels={};peak=0
    (out/'float').mkdir()
    for name,x in tracks.items():
        wavfile.write(out/'float'/(name+'.wav'),FS,x.astype(np.float32))
        write_pcm24(out/'full'/(name+'.wav'),matched[name])
        fs,pcm=wavfile.read(out/'full'/(name+'.wav'));assert fs==FS and len(pcm)==len(x) and pcm.dtype==np.int32
        decoded=pcm.astype(float)/2**31;peak=max(peak,float(np.max(abs(decoded))))
        levels[name]=lufs(np.concatenate([decoded[int(a*FS):int(b*FS)] for a,b in windows]))
    assert peak<1 and max(levels.values())-min(levels.values())<.12
    level_report={'windows_seconds':windows,'raw_lufs':measured_raw,'exported_speech_lufs':levels,
        'spread_lu':max(levels.values())-min(levels.values()),'peak':peak,
        'fixed_gain_db':{k:float(20*np.log10(v*common)) for k,v in factors.items()}}
    dump(out/'loudness.json',level_report)
    keys={};rng=np.random.default_rng(20261105);sections=[];metrics=[]
    with (out/'scores.csv').open('w') as f:
        writer=csv.writer(f);writer.writerow(['clip','option','heard_callsign_digits','room_or_echo_coloration','end_consonant_loss','hiss','musical_noise_or_pumping','preference','notes'])
        for clip,start,end,note in CLIPS:
            cut=slice(start*FS,end*FS);options=['dfn20','guard_v1','guard_release24','guard_smooth60'];rng.shuffle(options);keys[clip]={}
            section=[f'<section><h2>{html.escape(clip)} · {start}–{end} s</h2><p>{html.escape(note)}</p>']
            for i,name in enumerate(options):
                letter=chr(65+i);keys[clip][letter]=name
                write_pcm24(out/'clips'/clip/(letter+'.wav'),matched[name][cut])
                writer.writerow([clip,letter]+['']*7)
                section.append(f'<label>{letter}<audio preload="none" controls src="clips/{clip}/{letter}.wav"></audio></label>')
            sections.append('\n'.join(section)+'</section>')
            for name in ['guard_release24','guard_smooth60','guard_r24_s60']:
                write_pcm24(out/'pairs'/(clip+'_guard_v1_L_'+name+'_R.wav'),np.stack([matched['guard_v1'][cut],matched[name][cut]],axis=1))
            entry={'clip':clip,'seconds':[start,end]}
            for name,_x in tracks.items():
                y=matched[name][cut].astype(float);ref=matched['guard_v1'][cut].astype(float)
                row={'listening_rms_relative_guard_v1_db':float(20*np.log10(np.linalg.norm(y)/np.linalg.norm(ref)))}
                if name in diagnostics:
                    d=diagnostics[name]['frame_summary'];mask=(d[:,0]>=start)&(d[:,0]<end)
                    row.update(mean_speech_evidence=float(np.mean(d[mask,1])),
                        median_original_residual_ir_outside_4ms_fraction=float(np.median(d[mask,5])),
                        mean_adjacent_bin_gain_difference_db=float(np.mean(d[mask,6])))
                entry[name]=row
            entry['model_estimate_energy_ratio_to_input_db']=float(20*np.log10(np.linalg.norm(E[cut])/np.linalg.norm(X[cut])))
            metrics.append(entry)
    dump(out/'blind_key.json',keys);dump(out/'diagnostics.json',metrics)
    page='''<!doctype html><html lang="zh"><meta charset="utf-8"><meta name="viewport" content="width=device-width"><title>弱语音自然度对照</title><style>body{font:17px system-ui;max-width:1000px;margin:2em auto;padding:0 1em;background:#f7fafb;color:#17212a}section{padding:1em;border:1px solid #cbd5de;background:white;margin:1em 0;border-radius:8px}label{display:flex;align-items:center;gap:1em;margin:.8em}audio{width:90%}a{color:#185c88}</style>
<h1>弱语音：空房间感的针对性对照</h1><p>你反馈两版在弱语音时底噪较低，但略有空教室回声感。本轮保留 DFN20 与旧语音保护版，再分别只缩短增益释放时间、或只加强频率平滑。所有选项取自同一 IQ、相同原生解调／AGC，已对齐并按共同语音窗匹配响度。</p>
<p>重点比较“空房间感”、尾辅音、沙沙声与音乐噪声。字母顺序逐片段打乱，切换播放器保持片段时间。这里没有已确认原文，不计算抄收准确率。</p>
<p><a href="report.txt">方法和已有证据</a> · <a href="scores.csv">记录表</a> · <a href="blind_key.json">听完查看身份</a> · <a href="full/guard_r24_s60.wav">补充：两项同时修改</a></p>'''
    page+='\n'.join(sections)+'''<script>for(const s of document.querySelectorAll('section')){let prev=null;for(const a of s.querySelectorAll('audio'))a.addEventListener('play',()=>{if(prev&&prev!==a){const t=prev.currentTime;prev.pause();a.currentTime=Math.min(t,a.duration||Infinity)}prev=a})}</script></html>'''
    (out/'review.html').write_text(page)
    report=f'''用户反馈：DFN20、speech_focus3、speech_guard 在较强语音下表现接近；两版混合在弱语音时底噪较低，但有轻微空教室回声感。尚无呼号、数字或辅音正确率证据。本轮目标是定位这种听感的来源，并检查能否保留已有的主观压噪偏好。

四项主要对照：
dfn20：原生 DFN20 输出，作为用户认可的参照。
guard_v1：旧 speech_guard 的精确重放。转 float32 后与上轮 float/speech_guard.wav 逐样本完全相同。
guard_release24：只将增益释放时间从 80 ms 改为 24 ms，其他参数不变。
guard_smooth60：只给输出增益加入高斯频率平滑，sigma=60 Hz（约 1.92 个频点）；语音证据与 attack/release 历史不变。不是改变 IF 或音频通带。
guard_r24_s60：两项同时改，仅作补充，不混入主盲听的四个选项。

已核验：DFN10/20 恢复的混合成分独立预测 DFN13，重建相对误差 {mix_error:.2f} dB；恢复原输入的误差 {input_error:.2f} dB。常数增益的 WOLA 重建包含首尾样本、无额外延迟副本；单脉冲测试证实 80 ms 增益释放不会凭空复制出 80 ms 后的语音。这些检查排除了固定混合的对齐错误，不能排除自适应混合与原 DFN 的相位／多帧处理共同带来的听感。

机制假设：增益释放可能让某些频带的噪声在音节后持续较久；细窄、起伏的逐频点增益可能产生短暂振铃和染色。“空教室感”是用户听感描述，目前没有测得真实房间冲激响应，不能据此宣布存在某个混响时间。频谱加权引起听感杂音及频率平滑的研究背景见文末；本轮实现是自行定义的参数对照，不是论文算法复现。

diagnostics.json 的冻结增益冲激响应，只分析原输入残留支路。它测量每帧固定增益的 IR 中 |lag|>4 ms 的能量比例，不是整个非线性 DFN 的冲激响应，也不是人声混响量或可懂度分数。相邻频点增益差是谱面粗糙度诊断。声音变干净、IR 指标变小或总 RMS 变低，都不直接代表呼号更容易辨认。

风险取舍：缩短释放可能损伤尾辅音或带来抽动；频率平滑可能回带更多底噪，或减弱窄弱谐波的突出程度。因此保留单项对照，先看哪一项改善了主观自然度，再评估底噪与字音。本轮未使用去混响或语音生成模型。

试听使用上轮相同共同语音窗 {windows} 匹配响度；每条整段只用一个固定增益。实际 PCM24 测量差 {level_report['spread_lu']:.3f} LU，最高样本峰值 {peak:.6f}。32／8 ms WOLA 时间轴保持不变，实际流式接入仍需另预算约 32 ms 处理帧延迟。无 API 请求、硬件设置变化或生产 NR 替换。

来源资料：
https://www.iks.rwth-aachen.de/en/publications/publications-detail//esch09/

复现：OPENBLAS_NUM_THREADS=1 OMP_NUM_THREADS=1 nice -n 10 PYTHON tools/bench/ssb_nr/run_speech_naturalness.py --study BASE --previous BASE/speech-focus-v1 --out NEW_DIR
先运行 test_speech_focus.py。来源 SHA256、旧版精确重放检查、数值测试、音频校验和本次参数在 manifest.json 中。新的主观自然度与抄收收益尚待用户反馈。
'''
    (out/'report.txt').write_text(report)
    sources=[Path(__file__)]+[Path(__file__).with_name(n) for n in ['speech_focus.py','test_speech_focus.py','run_speech_focus.py','study.py','omlsa.py']]
    snapshot=out/'source';snapshot.mkdir()
    for file in sources:
        shutil.copyfile(file,snapshot/file.name)
    test=subprocess.run([sys.executable,str(snapshot/'test_speech_focus.py')],capture_output=True,text=True,check=True)
    (out/'numerical_tests.txt').write_text(test.stdout+test.stderr)
    wav_hashes={};count=0
    for path in [*(out/'full').glob('*.wav'),*(out/'clips').rglob('*.wav'),*(out/'pairs').glob('*.wav')]:
        with wave.open(str(path),'rb') as f:
            assert f.getsampwidth()==3 and f.getframerate()==FS
        fs,pcm=wavfile.read(path);assert fs==FS and pcm.dtype==np.int32 and np.max(abs(pcm.astype(float)))/2**31<1
        if path.parent.name=='full':
            assert len(pcm)==len(X)
        wav_hashes[str(path.relative_to(out))]=digest(path);count+=1
    assert count==33
    dump(out/'audio_sha256.json',wav_hashes)
    manifest={'created_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),
        'base_study':str(base),'previous_experiment':str(prior),'input_sha256':old['input_sha256'],
        'evidence_sha256':digest(evidence),'source_sha256':{f.name:digest(f) for f in sources},
        'configs':{k:asdict(v) for k,v in configs.items()},'seconds_per_candidate':timings,
        'old_guard_float32_bit_identical':True,'mix_reconstruction_error_db':float(mix_error),
        'input_reconstruction_error_db':float(input_error),'numerical_tests':'7 passed',
        'pcm24_files_checked':count,'loudness':level_report,'output_samples':len(X),'sample_rate':FS,
        'live_receiver_requests':0,'production_modified':False,'echo_cause_confirmed':False,
        'new_candidates_user_evaluated':False,'intelligibility_gain_established':False}
    dump(out/'manifest.json',manifest)
    print('Review:',out/'review.html',flush=True)


if __name__=='__main__':
    main()
