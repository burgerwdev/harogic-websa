"""Source mixture/timing tests; no speech intelligibility ground truth implied."""
import unittest

import numpy as np
from speech_focus import FocusConfig, enhance, recover_components


class Checks(unittest.TestCase):
    def setUp(self):
        rng=np.random.default_rng(44)
        self.x=rng.normal(size=12345)*.04
        self.e=np.sin(np.arange(len(self.x))*.13)*.05
        self.hz=np.fft.rfftfreq(512,1/16000)
        self.times=np.arange(-1,110)*.008
        self.probability=np.zeros((len(self.times),len(self.hz)))

    def test_recover_and_predict_independent_attenuation(self):
        def mix(db):
            a=10**(-db/20)
            return (1-a)*self.e+a*self.x
        x,e=recover_components(mix(10),mix(20))
        self.assertLess(np.max(abs(x-self.x)),1e-14)
        self.assertLess(np.max(abs(e-self.e)),1e-14)
        a=10**(-13/20)
        self.assertLess(np.max(abs((1-a)*e+a*x-mix(13))),1e-14)

    def test_constant_residual_reproduces_model_mix_at_boundaries(self):
        for smoothing in [0,60]:
            c=FocusConfig(noise_atten_db=13,speech_residual_atten_db=13,gain_smoothing_hz=smoothing)
            y,_=enhance(self.x,self.e,self.probability,self.times,self.hz,c)
            a=10**(-13/20)
            self.assertEqual(len(y),len(self.x))
            self.assertLess(np.max(abs(y-((1-a)*self.e+a*self.x))),1e-13)

    def test_silence_stays_zero_without_noise_pumping(self):
        x=np.zeros_like(self.x)
        y,d=enhance(x,x,self.probability,self.times,self.hz,FocusConfig(voice_boost_db=3))
        self.assertTrue(np.array_equal(x,y))
        self.assertTrue(np.all(np.isfinite(d['frame_summary'])))

    def test_future_audio_cannot_change_samples_before_framing_delay(self):
        a,_=enhance(self.x,self.e,self.probability,self.times,self.hz,FocusConfig(voice_boost_db=3))
        e=self.e.copy();e[8000:]*=-2
        b,_=enhance(self.x,e,self.probability,self.times,self.hz,FocusConfig(voice_boost_db=3))
        self.assertLess(np.max(abs(a[:6400]-b[:6400])),1e-14)

    def test_frequency_smoothing_reduces_only_rendered_mask_ringing(self):
        # Deliberately narrow, persistent evidence: the network is silent here.
        # The detector/envelope stays identical in both renders.
        p=self.probability.copy();p[:,30:33]=1
        x=np.random.default_rng(5).normal(size=len(self.x))*.05
        a,da=enhance(x,np.zeros_like(x),p,self.times,self.hz,FocusConfig())
        b,db=enhance(x,np.zeros_like(x),p,self.times,self.hz,FocusConfig(gain_smoothing_hz=60))
        self.assertTrue(np.array_equal(da['frame_summary'][:,:3],db['frame_summary'][:,:3]))
        self.assertLess(np.mean(db['frame_summary'][20:,5]),np.mean(da['frame_summary'][20:,5])*.25)
        self.assertGreater(np.max(abs(a-b)),1e-5)

    def test_impulse_cannot_produce_an_eighty_ms_delayed_audio_copy(self):
        x=np.zeros(12000);x[4000]=.5
        p=self.probability.copy();p[:,30:33]=1
        y,_=enhance(x,np.zeros_like(x),p,self.times,self.hz,FocusConfig())
        # Frequency-selective WOLA can spread an impulse within its window,
        # but an 80 ms gain release cannot create audio after that finite support.
        self.assertLess(np.max(abs(y[4000+1536:])),1e-14)

    def test_short_release_does_not_extend_envelope_after_evidence_ends(self):
        p=self.probability.copy();p[(self.times>=0)&(self.times<.12),20:80]=1
        x=np.random.default_rng(77).normal(size=len(self.x))*.05
        _,a=enhance(x,np.zeros_like(x),p,self.times,self.hz,FocusConfig())
        _,b=enhance(x,np.zeros_like(x),p,self.times,self.hz,FocusConfig(release_ms=24))
        tail=(a['frame_summary'][:,0]>.18)&(a['frame_summary'][:,0]<.25)
        self.assertLess(np.mean(b['frame_summary'][tail,1]),np.mean(a['frame_summary'][tail,1])*.5)


if __name__=='__main__':
    unittest.main()
