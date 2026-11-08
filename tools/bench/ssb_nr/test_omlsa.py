"""Numerical/streaming checks, not a claim about radio speech intelligibility."""
import unittest

import numpy as np
from omlsa import Config, OmlsaImcra, enhance


class Checks(unittest.TestCase):
    def test_wola_alignment_and_length_including_boundaries(self):
        rng = np.random.default_rng(42)
        x = rng.normal(size=12345)*.1
        x[[0, 513, -1]] = .8
        y, _ = enhance(x, Config(), unity=True)
        self.assertEqual(len(x), len(y))
        self.assertLess(np.max(abs(x-y)), 1e-12)

    def test_no_future_dependence_before_the_window_delay(self):
        rng = np.random.default_rng(93)
        x = rng.normal(size=16000)*.03
        a, _ = enhance(x, Config())
        x[8000:] = rng.normal(size=8000)*.8
        b, _ = enhance(x, Config())
        self.assertLess(np.max(abs(a[:7488]-b[:7488])), 1e-12)

    def test_two_pass_per_bin_noise_and_upward_tracking(self):
        tracker = OmlsaImcra(Config())
        rng = np.random.default_rng(18)
        shape = np.ones(257)*1e-4
        shape[50:100] *= 10
        estimates = []
        for n in range(1800):
            p = rng.exponential(size=257)*shape*(1 if n < 900 else 4)
            g, noise, _ = tracker.gain(p)
            self.assertTrue(np.all(np.isfinite(g)))
            self.assertGreaterEqual(np.min(g), 10**(-10/20)-1e-12)
            self.assertLessEqual(np.max(g), 1)
            estimates.append(noise)
        early = np.mean(estimates[600:900], axis=0)
        late = np.mean(estimates[1500:], axis=0)
        self.assertGreater(np.mean(early[55:95])/np.mean(early[110:150]), 6)
        ratio = np.median(late/early)
        self.assertTrue(2.5 < ratio < 5.5, ratio)

    def test_silence_is_finite_and_length_preserving(self):
        for profile in ['author','ssb']:
            x = np.zeros(19031)
            y, _ = enhance(x, Config(profile=profile))
            self.assertTrue(np.array_equal(x, y))


if __name__ == '__main__':
    unittest.main()
