#!/usr/bin/env python3
"""DFN-based speech protection and selective presence experiment on existing IQ replay.

Run with OPENBLAS_NUM_THREADS=1 OMP_NUM_THREADS=1 and nice -n 10 when a live
receiver shares the machine. Never reads/configures the live service.
"""
import argparse
import csv
import datetime
import html
import json
import re
import shutil
import subprocess
import sys
import time
from dataclasses import asdict
from pathlib import Path

import numpy as np
from scipy.io import wavfile
from speech_focus import FocusConfig, enhance, recover_components
from study import digest, dump, lufs, write_pcm24

FS=48000
CLIPS=[('strong_38',38,46,'较强语音：注意自然度与辅音'),
       ('weak_04',4,12,'弱语音候选：注意字音是否突出'),
       ('weak_118',118,125,'DFN 接近统一衰减的弱语音候选'),
       ('pause_108',108,114,'噪声主导候选：注意底噪抽动、啸叫、音乐噪声')]


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--study',type=Path,required=True)
    parser.add_argument('--out',type=Path,required=True)
    args=parser.parse_args();base=args.study.resolve();out=args.out.resolve()
    if out==base:
        raise ValueError('use an independent output directory')
    out.mkdir(parents=True,exist_ok=True)
    if any(out.iterdir()):
        raise ValueError('use a fresh output directory to preserve listening annotations')
    native=json.loads((base/'native/native.json').read_text())
    files={k:base/'native'/(k+'.f32') for k in ['none','dfn10','dfn13','dfn20','nr_input']}
    hashes={k:digest(v) for k,v in files.items()}
    assert all(r['effective']=='Dfn' for r in native['nr'] if r['name'].startswith('dfn'))
    x,y10,y13,y20=[np.fromfile(files[k],dtype='<f4').astype(float) for k in ['none','dfn10','dfn13','dfn20']]
    assert all(len(a)==native['output_samples'] and np.all(np.isfinite(a)) for a in [x,y10,y13,y20])
    X,E=recover_components(y10,y20)
    a=10**(-13/20);predicted=(1-a)*E+a*X
    prediction_error=float(20*np.log10(np.linalg.norm(predicted-y13)/np.linalg.norm(y13)))
    input_error=float(20*np.log10(np.linalg.norm(X-x)/np.linalg.norm(x)))
    assert prediction_error < -100 and input_error < -100, 'DFN mixing identity not established'
    # Store algebraic components as diagnostics, never as clean reference speech.
    E.astype('<f4').tofile(out/'neural_estimate.f32')
    source=None
    for path in base.glob('omlsa_*.npz'):
        with np.load(path) as d:
            cfg=json.loads(str(d['config_json']))
            if cfg['profile']=='ssb' and cfg['attenuation_db']==10:
                assert str(d['input_sha256'])==hashes['nr_input']
                probability=d['speech_probability'];times=d['time'];hz=d['frequency'];source=path
                break
    if source is None:
        raise ValueError('need the conservative 10 dB IMCRA diagnostics from the existing study')
    configs={
        'speech_guard':FocusConfig(),
        'speech_focus3':FocusConfig(voice_boost_db=3),
        'dfn_fixed_presence3':FocusConfig(noise_atten_db=13,speech_residual_atten_db=13,voice_boost_db=3,fixed_presence=True),
    }
    tracks={'dfn13':y13,'dfn20':y20};diagnostics={};timings={}
    for name,cfg in configs.items():
        start=time.monotonic();tracks[name],d=enhance(X,E,probability,times,hz,cfg)
        diagnostics[name]=d;timings[name]=time.monotonic()-start
        dump(out/(name+'_config.json'),d['config'])
        np.savez_compressed(out/(name+'_diagnostics.npz'),frame_summary=d['frame_summary'],columns=d['columns'])
        print(name,'processed',round(timings[name],2),'s',flush=True)
    windows=json.loads((base/'loudness.json').read_text())['windows_seconds']
    raw_lufs={name:lufs(np.concatenate([v[int(a*FS):int(b*FS)] for a,b in windows])) for name,v in tracks.items()}
    factors={name:10**((-23-value)/20) for name,value in raw_lufs.items()}
    peak=max(float(np.max(abs(x)))*factors[name] for name,x in tracks.items())
    common=min(1,10**(-2/20)/peak)
    matched={name:(x*factors[name]*common).astype(np.float32) for name,x in tracks.items()}
    (out/'float').mkdir(exist_ok=True)
    measured={};actual_peak=0
    for name,x in tracks.items():
        wavfile.write(out/'float'/(name+'.wav'),FS,x.astype(np.float32))
        write_pcm24(out/'full'/(name+'.wav'),matched[name])
        # Verify quantized full-track files, not just in-memory samples.
        fs,pcm=wavfile.read(out/'full'/(name+'.wav'));assert fs==FS and len(pcm)==len(x) and pcm.dtype==np.int32
        converted=pcm.astype(float)/2**31;actual_peak=max(actual_peak,float(np.max(abs(converted))))
        measured[name]=lufs(np.concatenate([converted[int(a*FS):int(b*FS)] for a,b in windows]))
    assert actual_peak < 1 and max(measured.values())-min(measured.values()) < .12
    levels={'common_speech_windows':windows,'raw_lufs':raw_lufs,'gain_db':{k:float(20*np.log10(v*common)) for k,v in factors.items()},
        'quantized_speech_lufs':measured,'spread_lu':max(measured.values())-min(measured.values()),'sample_peak_max':actual_peak}
    dump(out/'loudness.json',levels)
    rng=np.random.default_rng(20261104);keys={};sections=[];metrics=[]
    with (out/'scores.csv').open('w') as f:
        writer=csv.writer(f);writer.writerow(['clip','option','heard_callsign_digits','speech_clarity','consonants','distortion','hiss','pumping_or_musical_noise','preference','notes'])
        for clip,start,end,note in CLIPS:
            cut=slice(int(start*FS),int(end*FS));choices=['dfn13','dfn20','speech_guard','speech_focus3'];rng.shuffle(choices);keys[clip]={}
            section=[f'<section><h2>{html.escape(clip)} · {start}–{end} s</h2><p>{html.escape(note)}</p>']
            for i,name in enumerate(choices):
                letter=chr(65+i);keys[clip][letter]=name
                write_pcm24(out/'clips'/clip/(letter+'.wav'),matched[name][cut])
                writer.writerow([clip,letter]+['']*8)
                section.append(f'<label>{letter}<audio controls preload="none" src="clips/{clip}/{letter}.wav"></audio></label>')
            section.append('</section>');sections.append('\n'.join(section))
            for name in ['speech_guard','speech_focus3']:
                write_pcm24(out/'pairs'/(clip+'_dfn13_L_'+name+'_R.wav'),np.stack([matched['dfn13'][cut],matched[name][cut]],axis=1))
            entry={'clip':clip,'seconds':[start,end],'candidate_label':note}
            for name,v in matched.items():
                original=matched['dfn13'][cut].astype(float);processed=v[cut].astype(float)
                scale=float(np.dot(original,processed)/np.dot(original,original))
                entry[name]={'rms_difference_from_dfn13_db':float(20*np.log10(np.linalg.norm(processed)/np.linalg.norm(original))),
                    'residual_after_uniform_gain_db':float(20*np.log10(max(np.linalg.norm(processed-scale*original)/np.linalg.norm(processed),1e-30)))}
                if name in diagnostics:
                    d=diagnostics[name]['frame_summary'];mask=(d[:,0]>=start)&(d[:,0]<end)
                    entry[name]['mean_speech_evidence']=float(np.mean(d[mask,1]))
            metrics.append(entry)
    dump(out/'blind_key.json',keys);dump(out/'clip_diagnostics.json',metrics)
    header='''<!doctype html><html lang="zh"><meta charset="utf-8"><meta name="viewport" content="width=device-width"><title>DFN 语音增强离线试听</title><style>body{font:17px system-ui;max-width:1000px;margin:2em auto;padding:0 1em;background:#f7fafb;color:#17212a}section{padding:1em;border:1px solid #cbd5de;background:white;margin:1em 0;border-radius:8px}label{display:flex;align-items:center;gap:1em;margin:.8em}audio{width:90%}img{max-width:100%}a{color:#185c88}</style>
<h1>DFN：降底噪与语音增强</h1><p>目标：底噪更低，同时字音、辅音和弱人声更容易辨认。各条来自同一 IQ、同一原生解调／AGC，已对齐并在共同语音候选窗口匹配响度。</p>
<p>四个选项独立打乱：DFN13、DFN20、语音保护混合、语音保护＋选择性清晰度增强。标签仅是候选片段，不是已知原文。先写听到的内容，再比较人声失真、底噪和杂音。切换播放器保持片段播放位置。</p>
<p><a href="report.txt">方法与限制</a> · <a href="scores.csv">记录表</a> · <a href="blind_key.json">完成后查看身份</a> · <a href="full/dfn_fixed_presence3.wav">补充：DFN13＋固定 EQ 对照</a></p>'''
    script='''<script>for(const s of document.querySelectorAll('section')){let previous=null;for(const a of s.querySelectorAll('audio'))a.addEventListener('play',()=>{if(previous&&previous!==a){const t=previous.currentTime;previous.pause();a.currentTime=Math.min(t,a.duration||Infinity)}previous=a})}</script></html>'''
    (out/'review.html').write_text(header+'\n'.join(sections)+script)
    report=f'''本轮目标：同时降低背景沙沙声、提高真实语音与辅音的突出程度。上一轮用户反馈：DFN 总体听感更好，保守 OM-LSA 与 DFN 接近，author18 噪声最低但人声失真。因此继续以 DFN 为主体，未修改生产 NR。

已有代码已将 min_db_thresh 降至 -100 dB；不能将当前弱语音抑制简单归因于上游默认 -10 dB 的纯噪声开关。本轮未改变此阈值，也未重新运行硬件接收。处理仅使用前轮从同一 IQ、原生 new_prechannelized 与新 AGC 产生的 PCM。

DFN 的 atten_lim 是把模型估计 E 与输入 X 线性混合：Y_a=(1-a)E+aX，a=10^(-atten_db/20)。由 DFN10 与 DFN20 解出 X/E，再独立预测 DFN13。真实录音预测相对误差 {prediction_error:.2f} dB，恢复 X 与无 NR 输入相对误差 {input_error:.2f} dB，均通过 -100 dB 阈值。这是混合恒等式校验，不是输入 SNR 或干净语音恢复准确率。E 只是网络估计，可能漏音、失真，不能当作干净真值。

speech_guard：仍使用 DFN 神经网络语音估计作为主体；将其频谱证据与前轮 IMCRA 的连续语音证据结合。统计证据先作相邻频点与 24 ms 时间平均，再以 8 ms attack／80 ms release 平滑，避免硬门控。证据低时，原输入回混量按 20 dB 衰减；证据高时按 8 dB 衰减，以给模型漏掉的弱语音留出空间。8/20 dB 是原输入残留的混合系数，不是信号 SNR 收益，也不是最终每个频点的绝对衰减保证。

speech_focus3：在相同语音保护混合上，仅在有语音证据的时频位置作最多 +3 dB 的温和清晰度强调，频率权重平滑覆盖 500–2700 Hz，中部权重最高。没有整段提高总音量。补充 dfn_fixed_presence3 是相同最高 +3 dB 频率曲线作用于 DFN13 的固定 EQ 对照，用于区分选择性处理与音色偏好；它只在 full/ 中提供，未混入主比较的四个选项。

这里“语音证据”是估计，不是准确检测：IMCRA 可能把噪声峰判为语音，神经网络可能漏掉低 SNR／SSB 语音。过度强调可能带来沙沙声抽动、音乐噪声或新的音色失真；保留弱语音也可能回混更多噪声。当前尚未获得新候选的用户听辨结论，不能声称已经优于 DFN 或恢复了某个呼号。

音频主路径保持 48 kHz，用 1536 点／384 步长的 WOLA（32／8 ms）处理；音频没有经过前轮 16 kHz 输出桥接。只有统计证据取自前轮 16 kHz 分析，按准确相同的帧中心时刻映射；不会选择未来帧。离线输出按原样本时间放回，实时实现应在原有 DFN 的 30 ms 上另预算约 32 ms 的帧可用延迟，尚未实现生产流式接入。

所有试听在相同候选语音窗 {windows} 上用 ffmpeg BS.1770/R128 匹配，一条轨道只用一个固定增益，不单独放大弱片段。导出的 PCM24 实测响度差 {levels['spread_lu']:.3f} LU，最高样本峰值 {actual_peak:.6f}。float/ 保存未匹配响度的处理输出，full/、clips/、pairs/ 为留余量的试听版。

review.html 提供四组片段、每组四个随机字母选项。先独立记字音，再比较失真、背景沙沙声、抽动和偏好；无确认原文时不统计抄收准确率。左右声道配对文件的名字明确写出左 DFN13／右候选。32 ms 的窗口与本轮统计平滑是研究参数，未经过强度优化，不承诺固定收益。未使用 ASR 或语音生成模型补写音节。

原始方法背景：DeepFilterNet 论文描述神经语音包络与多帧复数滤波的语音周期性增强，本轮继续使用现有模型；这个混合控制与选择性强调是本地实验，不是该论文算法复现。另有降噪结合频谱塑形的听辨研究，但其听者／噪声条件不等同于弱 SSB，不能转用其收益数字。
https://www.isca-archive.org/interspeech_2023/schroter23b_interspeech.pdf
https://www.isca-archive.org/interspeech_2017/zorila17_interspeech.html

复现：OPENBLAS_NUM_THREADS=1 OMP_NUM_THREADS=1 nice -n 10 PYTHON tools/bench/ssb_nr/run_speech_focus.py --study BASE --out NEW_DIRECTORY。先运行 test_speech_focus.py；保留原 study 目录中的三档 DFN、无 NR PCM 和保守 IMCRA npz。manifest.json 记录来源和实现 SHA256。本轮不访问或重配接收机。
'''
    (out/'report.txt').write_text(report)
    source_files=[Path(__file__),Path(__file__).with_name('speech_focus.py'),Path(__file__).with_name('test_speech_focus.py'),Path(__file__).with_name('study.py'),Path(__file__).with_name('omlsa.py')]
    snapshot=out/'source';snapshot.mkdir(exist_ok=True)
    for file in source_files:
        shutil.copyfile(file,snapshot/file.name)
    test=subprocess.run([sys.executable,str(snapshot/'test_speech_focus.py')],capture_output=True,text=True,check=True)
    (out/'numerical_tests.txt').write_text(test.stdout+test.stderr)
    test_count=int(re.search(r'Ran (\d+) tests?',test.stdout+test.stderr).group(1))
    manifest={'created_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'base_study':str(base),
        'goal':'Lower background hiss while enhancing actual speech structure and consonants',
        'production_modified':False,'live_receiver_requests':0,'uses_existing_neural_model_estimates':True,
        'input_sha256':hashes,'statistical_evidence_file':str(source),'statistical_evidence_sha256':digest(source),
        'source_sha256':{file.name:digest(file) for file in source_files},'configs':{k:asdict(v) for k,v in configs.items()},
        'mix_identity_prediction_error_db':prediction_error,'mix_identity_input_error_db':input_error,
        'output_samples':len(x),'sample_rate':FS,'seconds_per_candidate':timings,
        'loudness':levels,'new_candidates_user_evaluated':False,'confirmed_transcript':None,
        'numerical_tests':f'{test_count} passed','pcm24_files':len(list(out.rglob('*.wav')))-len(tracks)}
    dump(out/'manifest.json',manifest)
    print('Review:',out/'review.html',flush=True)


if __name__=='__main__':
    main()
