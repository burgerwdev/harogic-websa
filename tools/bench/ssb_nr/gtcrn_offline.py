"""Adapters for the pinned official GTCRN reference and streaming ONNX models.

Only I/O compatibility is adapted: modern complex tensors and explicit original
ISTFT length. Model layers, weights, STFT window and amplitude are unchanged.
No AGC, level normalization, attenuation floor or postfilter is added here.
"""
import importlib.util
import sys
import time
from pathlib import Path

import numpy as np
import torch

RATE, FFT, HOP = 16000, 512, 256


def load_reference(repo, checkpoint):
    repo=Path(repo).resolve()
    name='_websa_official_gtcrn_'+str(abs(hash(str(repo))))
    if name not in sys.modules:
        spec=importlib.util.spec_from_file_location(name,repo/'gtcrn.py')
        module=importlib.util.module_from_spec(spec);sys.modules[name]=module;spec.loader.exec_module(module)
    model=sys.modules[name].GTCRN().cpu().eval()
    state=torch.load(checkpoint,map_location='cpu',weights_only=True)
    model.load_state_dict(state['model'],strict=True)
    return model, {'checkpoint_epoch':int(state['epoch']),
        'parameters_total':sum(p.numel() for p in model.parameters()),
        'parameters_trainable':sum(p.numel() for p in model.parameters() if p.requires_grad)}


def analyze(samples):
    samples=np.asarray(samples,dtype=np.float32)
    if samples.ndim!=1 or len(samples)<FFT or not np.all(np.isfinite(samples)):
        raise ValueError('expected finite mono 16 kHz PCM, at least one FFT window')
    audio=torch.from_numpy(np.ascontiguousarray(samples))
    window=torch.hann_window(FFT,periodic=True).sqrt()
    spectrum=torch.stft(audio,FFT,HOP,FFT,window,center=True,pad_mode='reflect',return_complex=True)
    return audio,window,spectrum


def synthesize(real_imag, window, length):
    value=torch.as_tensor(real_imag)
    complex_value=torch.view_as_complex(value.contiguous())
    output=torch.istft(complex_value,FFT,HOP,FFT,window,center=True,length=length)
    # The original infer.py omits length and can drop a partial final hop.
    # Preserve all samples; independently verify the shared legacy prefix.
    legacy=torch.istft(complex_value,FFT,HOP,FFT,window,center=True)
    prefix_error=float(torch.max(torch.abs(output[:len(legacy)]-legacy)))
    result=output.detach().cpu().numpy()
    if len(result)!=length or not np.all(np.isfinite(result)):
        raise ValueError('invalid reconstructed model output')
    return result, {'legacy_prefix_max_error':prefix_error,'legacy_length':len(legacy),
                    'preserved_final_samples':length-len(legacy)}


def infer_reference(model,samples):
    audio,window,spectrum=analyze(samples)
    started=time.perf_counter()
    with torch.inference_mode():
        estimate=model(torch.view_as_real(spectrum)[None])[0]
        output,checks=synthesize(estimate,window,len(audio))
        unity=torch.istft(spectrum,FFT,HOP,FFT,window,center=True,length=len(audio))
    checks.update(seconds=time.perf_counter()-started,frames=spectrum.shape[-1],
        unity_max_error=float(torch.max(torch.abs(unity-audio))),
        analysis_audio_offset_samples=0,frame_availability_budget_ms=32,
        mode='official full-sequence PyTorch reference; centered offline STFT')
    return output,checks


def run_onnx_spectrum(path,features):
    import onnxruntime as ort
    options=ort.SessionOptions();options.intra_op_num_threads=1;options.inter_op_num_threads=1
    session=ort.InferenceSession(str(path),sess_options=options,providers=['CPUExecutionProvider'])
    assert [i.name for i in session.get_inputs()]==['mix','conv_cache','tra_cache','inter_cache']
    features=np.asarray(features,dtype=np.float32)
    if features.ndim!=4 or features.shape[0]!=1 or features.shape[1]!=257 or features.shape[-1]!=2:
        raise ValueError('unexpected official ONNX feature shape')
    conv=np.zeros([2,1,16,16,33],dtype=np.float32)
    tra=np.zeros([2,3,1,1,16],dtype=np.float32)
    inter=np.zeros([2,1,33,16],dtype=np.float32)
    outputs=[];elapsed=[]
    for i in range(features.shape[2]):
        started=time.perf_counter_ns()
        result,conv,tra,inter=session.run(None,{
            'mix':np.ascontiguousarray(features[:,:,i:i+1,:]),
            'conv_cache':conv,'tra_cache':tra,'inter_cache':inter})
        elapsed.append((time.perf_counter_ns()-started)/1e6);outputs.append(result)
    return np.concatenate(outputs,axis=2), {'frame_count':len(outputs),
        'frame_mean_ms':float(np.mean(elapsed)),'frame_p99_ms':float(np.quantile(elapsed,.99)),
        'frame_max_ms':float(np.max(elapsed)),'frame_hop_ms':16,
        'caches_reset':'once at recording start; never at excerpt boundaries'}


def infer_onnx(path,samples):
    audio,window,spectrum=analyze(samples)
    started=time.perf_counter()
    estimate,checks=run_onnx_spectrum(path,torch.view_as_real(spectrum)[None].numpy())
    with torch.inference_mode():
        output,tail=synthesize(estimate[0],window,len(audio))
    checks.update(tail);checks.update(seconds=time.perf_counter()-started,
        analysis_audio_offset_samples=0,frame_availability_budget_ms=32,
        mode='official per-frame ONNX with retained caches; centered offline STFT/ISTFT')
    return output,checks


def compare_official_onnx(repo,samples):
    repo=Path(repo)
    root,_=load_reference(repo,repo/'checkpoints/model_trained_on_dns3.tar')
    paired,_=load_reference(repo,repo/'stream/onnx_models/model_trained_on_dns3.tar')
    states_equal=all(torch.equal(a,b) for a,b in zip(root.state_dict().values(),paired.state_dict().values(),strict=False))
    audio,window,spectrum=analyze(samples)
    features=torch.view_as_real(spectrum)[None]
    with torch.inference_mode():
        reference=paired(features).numpy()
    streamed,checks=run_onnx_spectrum(repo/'stream/onnx_models/gtcrn_simple.onnx',features.numpy())
    maximum=float(np.max(abs(reference-streamed)))
    relative=float(20*np.log10(max(np.linalg.norm(reference-streamed)/np.linalg.norm(reference),1e-30)))
    if maximum>2e-4 or relative>-80:
        raise ValueError(f'paired official reference/ONNX mismatch: {maximum}, {relative} dB')
    return {'root_and_stream_dns3_states_identical':states_equal,
        'paired_spectrum_max_error':maximum,'paired_spectrum_relative_error_db':relative,
        'note':'Different root/stream checkpoints must be treated as different models. Spectral cache parity does not certify live PCM framing or production integration.',
        **checks}
