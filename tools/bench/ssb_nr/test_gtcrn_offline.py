"""Official-weight parity, time alignment and causal support checks."""
import argparse
import sys
import unittest
import warnings
from pathlib import Path

import numpy as np
import torch
from gtcrn_offline import analyze, compare_official_onnx, infer_reference, load_reference
from scipy import signal

REPO=None


class Checks(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        torch.set_num_threads(1);torch.set_num_interop_threads(1)
        cls.model,_=load_reference(REPO,Path(REPO)/'checkpoints/model_trained_on_dns3.tar')

    def test_stft_preserves_impulse_positions_and_partial_final_hop(self):
        x=np.random.default_rng(9).normal(size=12345).astype(np.float32)*.02
        x[[0,7001,-1]]=.8
        audio,window,spectrum=analyze(x)
        y=torch.istft(spectrum,512,256,512,window,center=True,length=len(x)).numpy()
        self.assertEqual(len(y),len(x))
        self.assertLess(np.max(abs(x-y)),3e-7)
        self.assertEqual(np.argmax(abs(y[6900:7100]))+6900,7001)

    def test_modern_features_equal_official_real_imaginary_interface(self):
        x=np.random.default_rng(91).normal(size=16000).astype(np.float32)*.03
        audio,w,s=analyze(x)
        with warnings.catch_warnings():
            warnings.simplefilter('ignore')
            official=torch.stft(audio,512,256,512,w,return_complex=False)
        self.assertTrue(torch.equal(official,torch.view_as_real(s)))

    def test_model_length_and_legacy_prefix(self):
        x=np.random.default_rng(15).normal(size=19313).astype(np.float32)*.03
        y,m=infer_reference(self.model,x)
        self.assertEqual(len(x),len(y))
        self.assertTrue(np.all(np.isfinite(y)))
        self.assertLess(m['legacy_prefix_max_error'],1e-7)
        self.assertGreater(m['preserved_final_samples'],0)

    def test_no_future_audio_before_the_documented_stft_support(self):
        x=np.random.default_rng(24).normal(size=48000).astype(np.float32)*.03
        a,_=infer_reference(self.model,x)
        x[32000:]*=-2
        b,_=infer_reference(self.model,x)
        self.assertLess(np.max(abs(a[:32000-512]-b[:32000-512])),3e-6)

    def test_matching_stream_checkpoint_and_onnx_have_same_spectra(self):
        x=np.random.default_rng(25).normal(size=16384).astype(np.float32)*.03
        result=compare_official_onnx(REPO,x)
        self.assertLess(result['paired_spectrum_relative_error_db'],-80)

    def test_rate_bridge_has_no_sample_offset(self):
        x=np.zeros(48000);x[24000]=1
        y=signal.resample_poly(signal.resample_poly(x,1,3),3,1)
        self.assertEqual(len(x),len(y))
        self.assertEqual(np.argmax(abs(y)),24000)


if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('--repo',type=Path,required=True)
    args,remaining=parser.parse_known_args();REPO=args.repo.resolve()
    unittest.main(argv=[sys.argv[0]]+remaining)
