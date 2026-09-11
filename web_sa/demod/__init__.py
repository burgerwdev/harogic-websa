"""
demod -- SDR-mode DSP building blocks (numpy + vendor DSP_DDC).

Public pieces:
  * DdcChannel  -- DSP_DDC channelizer (complex-float output)
  * AnalogDemod -- AM/FM/NFM/WFM/USB/LSB/CW demodulators
  * Panadapter  -- wideband FFT for the spectrum + waterfall
"""
from __future__ import annotations

from .ddc import DdcChannel
from .demod import ANALOG_MODES, AnalogDemod
from .spectrum import Panadapter

__all__ = ['ANALOG_MODES', 'AnalogDemod', 'DdcChannel', 'Panadapter']
