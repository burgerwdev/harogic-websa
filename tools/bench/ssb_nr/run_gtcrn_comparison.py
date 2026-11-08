#!/usr/bin/env python3
"""Compare official GTCRN weights/ONNX with native DFN20 and guard on identical IQ.

No receiver API requests or production NR changes. Use single CPU threads and
nice -n 10; GTCRN uses the pre-volume native AGC PCM, not user DFN audio.
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
import wave
from pathlib import Path

import numpy as np
import torch
from gtcrn_offline import compare_official_onnx, infer_onnx, infer_reference, load_reference
from run_speech_focus import CLIPS
from scipy import signal
from scipy.io import wavfile
from study import digest, dump, lufs, write_pcm24

FS=48000
MODEL_NAMES=['gtcrn_dns3','gtcrn_vctk','gtcrn_stream_dns3']


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--study',type=Path,required=True)
    parser.add_argument('--guard-study',type=Path,required=True)
    parser.add_argument('--gtcrn-repo',type=Path,required=True)
    parser.add_argument('--out',type=Path,required=True)
    parser.add_argument('--reuse-inference',action='store_true',help='reuse hash-verified model outputs in an incomplete experiment')
    args=parser.parse_args();base=args.study.resolve();guard=args.guard_study.resolve();up=args.gtcrn_repo.resolve();out=args.out.resolve()
    if (out/'manifest.json').exists() or (out/'scores.csv').exists():
        raise ValueError('choose a fresh output directory to preserve completed results/annotations')
    if out.exists() and not args.reuse_inference:
        raise ValueError('existing directory requires --reuse-inference')
    subprocess.run(['git','diff','--exit-code','HEAD','--'],cwd=up,check=True,capture_output=True)
    commit=subprocess.check_output(['git','rev-parse','HEAD'],cwd=up,text=True).strip()
    out.mkdir(parents=True,exist_ok=True)
    torch.set_num_threads(1);torch.set_num_interop_threads(1)
    baseline=json.loads((guard/'manifest.json').read_text())
    paths={k:base/'native'/(k+'.f32') for k in ['nr_input','none','dfn20']}
    for k,p in paths.items():
        assert digest(p)==baseline['input_sha256'][k],k
    agc=np.fromfile(paths['nr_input'],dtype='<f4')
    none=np.fromfile(paths['none'],dtype='<f4')
    assert np.array_equal(none,agc*2),'volume must be applied after NR'
    down=signal.resample_poly(agc.astype(float),1,3).astype(np.float32)
    down_hash=__import__('hashlib').sha256(down.tobytes()).hexdigest()
    adapter_hash=digest(Path(__file__).with_name('gtcrn_offline.py'))
    inference={};tracks={}
    checkpoints={'gtcrn_dns3':up/'checkpoints/model_trained_on_dns3.tar',
        'gtcrn_vctk':up/'checkpoints/model_trained_on_vctk.tar',
        'gtcrn_stream_dns3':up/'stream/onnx_models/gtcrn_simple.onnx'}
    for name,checkpoint in checkpoints.items():
        meta=out/(name+'_inference.json');pcm=out/(name+'.f32');low=out/(name+'_16k.f32')
        if args.reuse_inference and meta.exists():
            m=json.loads(meta.read_text())
            assert m['input_sha256']==baseline['input_sha256']['nr_input']
            assert m['checkpoint_sha256']==digest(checkpoint) and m['adapter_sha256']==adapter_hash
            assert m['input_16k_sha256']==down_hash
            assert m['output_48k_sha256']==digest(pcm) and m['output_16k_sha256']==digest(low)
            y=np.fromfile(pcm,dtype='<f4');assert len(y)==len(agc) and np.all(np.isfinite(y))
            inference[name]=m;tracks[name]=y
        else:
            started=time.monotonic()
            if name=='gtcrn_stream_dns3':
                output,checks=infer_onnx(checkpoint,down)
                state=torch.load(up/'stream/onnx_models/model_trained_on_dns3.tar',map_location='cpu',weights_only=True)
                info={'checkpoint_epoch':int(state['epoch'])}
            else:
                model,info=load_reference(up,checkpoint);output,checks=infer_reference(model,down)
            y=signal.resample_poly(output.astype(float),3,1)[:len(agc)]*2
            assert len(y)==len(agc) and np.all(np.isfinite(y))
            output.astype('<f4').tofile(low);y.astype('<f4').tofile(pcm)
            m={'name':name,'input_sha256':baseline['input_sha256']['nr_input'],
                'checkpoint':str(checkpoint),'checkpoint_sha256':digest(checkpoint),
                'adapter_sha256':adapter_hash,'input_16k_sha256':down_hash,
                'output_16k_samples':len(output),'output_48k_samples':len(y),
                'output_16k_sha256':digest(low),'output_48k_sha256':digest(pcm),
                'wall_seconds':time.monotonic()-started,'model':info,'checks':checks}
            dump(meta,m);inference[name]=m;tracks[name]=y.astype(np.float32)
        print(name,'ready',flush=True)
    # Independently compare the official ONNX export with its matching weights,
    # NOT with the different root DNS3 checkpoint.
    parity=compare_official_onnx(up,down[:32000]);dump(out/'model_parity.json',parity)
    tracks['dfn20']=np.fromfile(paths['dfn20'],dtype='<f4')
    tracks['guard_smooth60']=np.fromfile(guard/'guard_smooth60.f32',dtype='<f4')
    tracks['guard_release24']=np.fromfile(guard/'guard_release24.f32',dtype='<f4')
    tracks['none_native']=none
    bridge=signal.resample_poly(down.astype(float),3,1)[:len(agc)]*2
    tracks['bridge16_control']=bridge.astype(np.float32)
    for name in MODEL_NAMES:
        # Explicit supplementary controls with the same 20 dB residual mixture
        # policy as DFN20. The main GTCRN options remain the official raw outputs.
        tracks[name+'_mix20']=(.9*tracks[name]+.1*bridge).astype(np.float32)
    assert all(len(x)==len(agc) and np.all(np.isfinite(x)) for x in tracks.values())
    # Isolate the resampling bridge, rather than crediting removed images as NR.
    bp=signal.butter(6,[270,2700],btype='bandpass',fs=FS,output='sos')
    error=bridge-none
    bridge_metrics={'full_band_relative_error_db':float(20*np.log10(np.linalg.norm(error[FS:-FS])/np.linalg.norm(none[FS:-FS]))),
        'speech_band_270_2700_relative_error_db':float(20*np.log10(np.linalg.norm(signal.sosfilt(bp,error)[FS:-FS])/np.linalg.norm(signal.sosfilt(bp,none)[FS:-FS]))),
        'interpretation':'Reconstruction error relative to the same input, not input/output SNR or speech recovery accuracy.'}
    dump(out/'bridge.json',bridge_metrics)
    windows=baseline['loudness']['windows_seconds']
    raw_lufs={k:lufs(np.concatenate([x[int(a*FS):int(b*FS)] for a,b in windows])) for k,x in tracks.items()}
    if not all(np.isfinite(v) for v in raw_lufs.values()):
        raise ValueError('cannot match silent model outputs')
    factors={k:10**((-23-v)/20) for k,v in raw_lufs.items()}
    peak=max(float(np.max(abs(x)))*factors[k] for k,x in tracks.items());common=min(1,10**(-2/20)/peak)
    matched={k:(x*factors[k]*common).astype(np.float32) for k,x in tracks.items()}
    (out/'float').mkdir(exist_ok=True);metered={};peak=0
    for name,x in tracks.items():
        wavfile.write(out/'float'/(name+'.wav'),FS,x.astype(np.float32))
        write_pcm24(out/'full'/(name+'.wav'),matched[name])
        fs,pcm=wavfile.read(out/'full'/(name+'.wav'));assert fs==FS and pcm.dtype==np.int32 and len(pcm)==len(agc)
        actual=pcm.astype(float)/2**31;peak=max(peak,float(np.max(abs(actual))))
        metered[name]=lufs(np.concatenate([actual[int(a*FS):int(b*FS)] for a,b in windows]))
    spread=max(metered.values())-min(metered.values());assert peak<1 and spread<.12
    levels={'windows_seconds':windows,'raw_lufs':raw_lufs,'exported_speech_lufs':metered,
        'spread_lu':spread,'sample_peak_max':peak,
        'fixed_gain_db':{k:float(20*np.log10(v*common)) for k,v in factors.items()}}
    dump(out/'loudness.json',levels)
    main_options=['dfn20','guard_smooth60',*MODEL_NAMES]
    sections=[];keys={};metrics=[];rng=np.random.default_rng(20261106)
    with (out/'scores.csv').open('w') as f:
        writer=csv.writer(f);writer.writerow(['clip','option','heard_callsign_digits','consonants','syllable_loss','room_coloration','hiss','musical_noise','preference','notes'])
        for clip,start,end,note in CLIPS:
            cut=slice(start*FS,end*FS);options=main_options.copy();rng.shuffle(options);keys[clip]={}
            section=[f'<section><h2>{html.escape(clip)} · {start}–{end} s</h2><p>{html.escape(note)}</p>']
            for i,name in enumerate(options):
                label=chr(65+i);keys[clip][label]=name
                write_pcm24(out/'clips'/clip/(label+'.wav'),matched[name][cut]);writer.writerow([clip,label]+['']*8)
                section.append(f'<label>{label}<audio preload="none" controls src="clips/{clip}/{label}.wav"></audio></label>')
            sections.append('\n'.join(section)+'</section>')
            for reference in ['dfn20','guard_smooth60']:
                for name in MODEL_NAMES:
                    pair=np.stack([matched[reference][cut],matched[name][cut]],axis=1)
                    write_pcm24(out/'pairs'/(clip+'_'+reference+'_L_'+name+'_R.wav'),pair)
            entry={'clip':clip,'seconds':[start,end]}
            original=matched['dfn20'][cut].astype(float)
            for name in main_options:
                x=matched[name][cut].astype(float);k=float(np.dot(x,original)/np.dot(original,original))
                entry[name]={'listening_rms_relative_dfn20_db':float(20*np.log10(np.linalg.norm(x)/np.linalg.norm(original))),
                    'uniform_fit_residual_db':float(20*np.log10(max(np.linalg.norm(x-k*original)/np.linalg.norm(x),1e-30))),
                    'note':'Mixture diagnostics, not SNR or intelligibility scores.'}
            metrics.append(entry)
    dump(out/'blind_key.json',keys);dump(out/'clip_diagnostics.json',metrics)
    supplementary=''.join(f'<li><a href="full/{k}.wav">{html.escape(k)}</a></li>' for k in tracks if k not in main_options)
    page='''<!doctype html><html lang="zh"><meta charset="utf-8"><meta name="viewport" content="width=device-width"><title>GTCRN／DFN／guard 离线比较</title><style>body{font:17px system-ui;max-width:1000px;margin:2em auto;padding:0 1em;background:#f7fafb;color:#17212a}section{padding:1em;border:1px solid #cbd5de;background:white;margin:1em 0;border-radius:8px}label{display:flex;align-items:center;gap:1em;margin:.7em}audio{width:90%}a{color:#185c88}</style>
<h1>官方 GTCRN 与 DFN／guard</h1><p>五个选项：DFN20、guard_smooth60、GTCRN 根目录 DNS3、VCTK、官方流式 DNS3。每片段字母独立打乱。三份 GTCRN 是直接模型输出，未叠加 guard、语音生成或抄收文本。</p>
<p>全部来自同一 IQ、相同原生解调和 AGC；模型前音量相同，模型后统一 volume 2.0，再按共同语音窗匹配试听响度。先独立听呼号／数字，再比较辅音、吞字、空房间感、底噪。切换播放器保持片段时间。</p>
<p><a href="report.txt">实现、延迟与限制</a> · <a href="scores.csv">记录表</a> · <a href="blind_key.json">听完查看身份</a></p>'''
    page+='\n'.join(sections)+'<details><summary>补充对照：release24、重采样及显式 20 dB 回混</summary><ul>'+supplementary+'</ul></details>'
    page+='''<script>for(const s of document.querySelectorAll('section')){let prev=null;for(const a of s.querySelectorAll('audio'))a.addEventListener('play',()=>{if(prev&&prev!==a){const t=prev.currentTime;prev.pause();a.currentTime=Math.min(t,a.duration||Infinity)}prev=a})}</script></html>'''
    (out/'review.html').write_text(page)
    # Source copies freeze the exact local adapter/tests and upstream code/license.
    source=out/'source';source.mkdir(exist_ok=True)
    local=[Path(__file__)]+[Path(__file__).with_name(n) for n in ['gtcrn_offline.py','test_gtcrn_offline.py','requirements_gtcrn.txt','requirements.txt','study.py','omlsa.py','run_speech_focus.py','speech_focus.py']]
    for file in local:
        shutil.copyfile(file,source/file.name)
    upstream_names=['gtcrn.py','infer.py','LICENSE','README.md','requirements.txt','stream/README.md','stream/gtcrn_stream.py','stream/modules/convert.py','stream/modules/convolution.py']
    upstream_hashes={}
    for name in upstream_names:
        file=up/name;dest=out/'upstream_source'/name;dest.parent.mkdir(parents=True,exist_ok=True);shutil.copyfile(file,dest);upstream_hashes[name]=digest(file)
    test=subprocess.run([sys.executable,str(source/'test_gtcrn_offline.py'),'--repo',str(up)],capture_output=True,text=True,check=True)
    (out/'numerical_tests.txt').write_text(test.stdout+test.stderr)
    ntests=int(re.search(r'Ran (\d+) tests?',test.stdout+test.stderr).group(1))
    audio_hashes={};counts={'full':0,'clips':0,'pairs':0}
    lengths={clip:(end-start)*FS for clip,start,end,_ in CLIPS}
    for kind in counts:
        for file in (out/kind).rglob('*.wav'):
            with wave.open(str(file),'rb') as f:
                assert f.getsampwidth()==3 and f.getframerate()==FS
                if kind=='full':
                    assert f.getnframes()==len(agc) and f.getnchannels()==1
                elif kind=='clips':
                    assert f.getnframes()==lengths[file.parent.name] and f.getnchannels()==1
                else:
                    clip=next(c for c in lengths if file.name.startswith(c+'_'));assert f.getnframes()==lengths[clip] and f.getnchannels()==2
            audio_hashes[str(file.relative_to(out))]=digest(file);counts[kind]+=1
    assert counts=={'full':11,'clips':20,'pairs':24},counts
    dump(out/'audio_sha256.json',audio_hashes)
    runtime={'python':sys.version,'torch':torch.__version__,'numpy':np.__version__,
        'onnxruntime':__import__('onnxruntime').__version__,'scipy':__import__('scipy').__version__,
        'torch_threads':torch.get_num_threads(),'torch_interop_threads':torch.get_num_interop_threads(),
        'torch_cpu_only':torch.version.cuda is None}
    verification={'passed':True,'numerical_tests':f'{ntests} passed','files_checked':counts,
        'same_pre_volume_agc_input':True,'matching_stream_model_graph':True,
        'output_samples_per_track':len(agc),'loudness_spread_lu':spread,'sample_peak_max':peak,
        'new_candidates_user_evaluated':False,'confirmed_transcript':None,
        'intelligibility_gain_established':False,'production_changed':False,'live_receiver_requests':0}
    dump(out/'verification.json',verification)
    manifest={'created_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),
        'base_study':str(base),'guard_study':str(guard),'official_repository':'https://github.com/Xiaobin-Rong/gtcrn',
        'official_commit':commit,'official_license':'MIT','input_sha256':{k:digest(p) for k,p in paths.items()},
        'guard_source_sha256':{k:digest(guard/(k+'.f32')) for k in ['guard_smooth60','guard_release24']},
        'local_source_sha256':{f.name:digest(f) for f in local},'upstream_source_sha256':upstream_hashes,
        'inference':inference,'stream_parity':parity,'runtime':runtime,'bridge':bridge_metrics,
        'loudness':levels,'verification':verification}
    dump(out/'manifest.json',manifest)
    report=f'''已完成官方 GTCRN 与同一 IQ 的原生 DFN20、guard_smooth60 离线比较材料。尚无本轮新模型的人工听辨结论，不能宣称哪个更清楚或 SNR 提升；确认原文仍为空。先用 review.html 比较呼号、数字、辅音、吞字、空房间感与底噪。

官方来源 https://github.com/Xiaobin-Rong/gtcrn ，固定提交 {commit}，MIT 许可；源码和许可另存 upstream_source/，预训练模型 SHA256 见每项 inference.json。输入是既有 native/nr_input.f32，即同一份 IQ 经 AnalogDemod::new_prechannelized 与新 AGC 后、volume 前的单声道 PCM。未使用用户 DFN 音频作模型输入或干净真值。全部模型推理后才统一乘 volume 2.0；AGC、解调与硬件设置没有变化。

三项独立的官方模型：
gtcrn_dns3：根目录 checkpoints/model_trained_on_dns3.tar，epoch 87，按 infer.py 的主模型路径。
gtcrn_vctk：根目录 checkpoints/model_trained_on_vctk.tar，epoch 92。
gtcrn_stream_dns3：官方 stream/onnx_models/gtcrn_simple.onnx，保留每帧缓存、整条录音只在开头清零一次；配套 stream/onnx_models/model_trained_on_dns3.tar 为 epoch 96。
根目录和流式目录 DNS3 的权重内容不同，不能视为同一个模型。与正确配套的参考模型比对后，官方 ONNX 频谱最大误差 {parity['paired_spectrum_max_error']:.3g}，相对重建误差 {parity['paired_spectrum_relative_error_db']:.2f} dB。这是实现一致性检查，不是 SNR 或语音重建质量分数。

官方 GTCRN 使用 16 kHz，512 点（32 ms）、256 步长（16 ms）、周期 Hann 开平方窗口。保留其居中 reflect STFT 和官方网络；只用现代 torch.view_as_real/view_as_complex 兼容旧接口，并在 ISTFT 显式给出原样本长度，避免丢掉末尾不足一跳的部分。单元检查证实新旧特征表示相同，显式长度输出与原脚本共有前缀一致。参数总计 48245，其中训练参数 23669；没有重新训练、调权重、额外 AGC 或模型前归一化。

48→16→48 kHz 使用 scipy 多相重采样，模型在 16 kHz 连续处理整条录音，输出再精确裁到原 48 kHz 样本数 {len(agc)}。单独保存 none_native 与 bridge16_control 对照。占用语音带 270–2700 Hz 的桥接相对重建误差 {bridge_metrics['speech_band_270_2700_relative_error_db']:.2f} dB，全频 {bridge_metrics['full_band_relative_error_db']:.2f} dB；后者主要包含原重采样镜像的变化，不记作降噪收益。

延迟：本轮居中离线 STFT/ISTFT 把输出放回原样本坐标，没有人为时间扭曲或拟合时移。DFN20 仍是前轮已校正 30 ms NR 延迟的原生输出；guard 沿用其原对齐。GTCRN 实时实现应按 32 ms 分析窗口和 16 ms 调度跳评估可用延迟，居中分析包含未来样本。已验证的是模型逐帧 ONNX 缓存与配套参考频谱相同，尚未完成现场 PCM 流式边界、设备时钟或原生 Rust 接入验证，不能把离线对齐当作零延迟。

主试听五项：DFN20、guard_smooth60，以及三项未经额外处理的官方 GTCRN 输出。你认为 guard_release24 与 smooth60 接近，因此 release24 另放 full/。*_mix20 只作补充控制：0.9×模型输出＋0.1×同一桥接输入，显式对应 DFN20 的残留回混策略；不混入主比较，也不冒充官方原始 GTCRN 结果。没有叠加 guard 或生成语音补写音节。

试听响度使用相同候选语音窗 {windows} 的 BS.1770/R128 表，每条整段只应用一个固定增益；不单独归一化弱语音／停顿。实际 PCM24 文件响度差 {spread:.3f} LU，最高样本峰值 {peak:.6f}。float/ 保存未匹配响度的模型母版，full/、clips/、pairs/ 为留余量的试听版。左右声道配对的文件名明确左参照／右 GTCRN。scores.csv 是空白听辨记录；无已确认原文，不统计抄收正确率。

本机单线程 CPU 耗时、官方 ONNX 每帧耗时与 p99 在 inference.json；nice 优先级下的离线 PyTorch 整段吞吐和 Rust DFN、ONNX 逐帧的计时口径不同，不作为公平的性能榜，也不承诺现场实时容量。CPU 环境与依赖版本已记录 runtime 与 python-freeze.txt。

{ntests} 项数值检查覆盖 STFT 首尾与时间位置、旧新张量接口、ISTFT 尾部长度、模型未来支持范围、配套 ONNX 缓存谱一致性、重采样时移。{sum(counts.values())} 份 PCM24 的格式、长度、来源校验和响度通过检查。静音更多、RMS 更低、谱图更暗都不是可懂度收益；没有把任何去噪输出当真值计算 SNR、SI-SDR、PESQ 或 STOI。

复现：使用 requirements_gtcrn.txt 建立独立 Python 3.12 CPU 环境；固定官方提交和校验权重后运行 run_gtcrn_comparison.py --study BASE --guard-study BASE/speech-naturalness-v2 --gtcrn-repo UPSTREAM --out NEW_DIR。可用 --reuse-inference 恢复不完整任务，但会核验输入、适配器、权重和缓存输出 SHA256。完成目录及评分表不会覆盖。生产 NR 未替换，本轮无接收机 API 请求。

主要官方资料：
https://github.com/Xiaobin-Rong/gtcrn/blob/{commit}/README.md
https://github.com/Xiaobin-Rong/gtcrn/blob/{commit}/infer.py
https://github.com/Xiaobin-Rong/gtcrn/blob/{commit}/stream/gtcrn_stream.py
'''
    (out/'report.txt').write_text(report)
    (out/'python-freeze.txt').write_text(subprocess.check_output([sys.executable,'-m','pip','freeze'],text=True))
    print('Review:',out/'review.html',flush=True)


if __name__=='__main__':
    main()
