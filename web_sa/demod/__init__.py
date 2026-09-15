"""
demod -- SDR-mode DSP building blocks (numpy + vendor DSP_DDC).

Public pieces:
  * DdcChannel  -- DSP_DDC channelizer (complex-float output)
  * AnalogDemod -- AM/FM/NFM/WFM/USB/LSB/CW demodulators
  * Panadapter  -- wideband FFT for the spectrum + waterfall
  * digital     -- symbol-level DSP for PSK/QAM (numpy only)
  * vector      -- Tier 1 vector measurements for VSA mode (numpy only)
"""
from __future__ import annotations

from . import digital, vector
from .ddc import DdcChannel
from .demod import ANALOG_MODES, AnalogDemod
from .spectrum import Panadapter

__all__ = ['ANALOG_MODES', 'AnalogDemod', 'DdcChannel', 'Panadapter', 'digital', 'vector']
