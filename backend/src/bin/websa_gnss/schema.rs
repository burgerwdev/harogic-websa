// SPDX-License-Identifier: BSD-3-Clause
// SPDX-FileCopyrightText: 2018-2020 Carles Fernandez-Prades <carles.fernandez@cttc.es>
// SPDX-FileCopyrightText: 2021 Carles Fernandez-Prades <carles.fernandez@cttc.es>
// Rust wire declarations adapted from GNSS-SDR v0.0.20 docs/protobuf/
// gnss_synchro.proto, monitor_pvt.proto, and nav_message.proto. Unknown fields
// are deliberately left to prost; these declarations retain the upstream tags.
//
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions are met:
// 1. Redistributions of source code must retain the above copyright notice,
//    this list of conditions and the following disclaimer.
// 2. Redistributions in binary form must reproduce the above copyright notice,
//    this list of conditions and the following disclaimer in the documentation
//    and/or other materials provided with the distribution.
// 3. Neither the name of the copyright holder nor the names of its contributors
//    may be used to endorse or promote products derived from this software
//    without specific prior written permission.
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
// AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
// IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
// ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE
// LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR
// CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF
// SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS
// INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN
// CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE)
// ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE
// POSSIBILITY OF SUCH DAMAGE.

#[derive(Clone, PartialEq, prost::Message)]
pub struct Observables {
    #[prost(message, repeated, tag = "1")]
    pub observable: Vec<GnssSynchro>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct GnssSynchro {
    #[prost(string, tag = "1")]
    pub system: String,
    #[prost(string, tag = "2")]
    pub signal: String,
    #[prost(uint32, tag = "3")]
    pub prn: u32,
    #[prost(int32, tag = "4")]
    pub channel_id: i32,
    #[prost(double, tag = "5")]
    pub acq_delay_samples: f64,
    #[prost(double, tag = "6")]
    pub acq_doppler_hz: f64,
    #[prost(uint64, tag = "7")]
    pub acq_samplestamp_samples: u64,
    #[prost(bool, tag = "9")]
    pub flag_valid_acquisition: bool,
    #[prost(double, tag = "13")]
    pub cn0_db_hz: f64,
    #[prost(double, tag = "14")]
    pub carrier_doppler_hz: f64,
    #[prost(uint64, tag = "17")]
    pub tracking_sample_counter: u64,
    #[prost(bool, tag = "18")]
    pub flag_valid_symbol_output: bool,
    #[prost(bool, tag = "20")]
    pub flag_valid_word: bool,
    #[prost(uint32, tag = "21")]
    pub tow_at_current_symbol_ms: u32,
    #[prost(double, tag = "22")]
    pub pseudorange_m: f64,
    #[prost(double, tag = "23")]
    pub rx_time: f64,
    #[prost(bool, tag = "24")]
    pub flag_valid_pseudorange: bool,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct MonitorPvt {
    #[prost(uint32, tag = "1")]
    pub tow_at_current_symbol_ms: u32,
    #[prost(uint32, tag = "2")]
    pub week: u32,
    #[prost(double, tag = "3")]
    pub rx_time: f64,
    #[prost(double, tag = "17")]
    pub latitude: f64,
    #[prost(double, tag = "18")]
    pub longitude: f64,
    #[prost(double, tag = "19")]
    pub height: f64,
    #[prost(uint32, tag = "20")]
    pub valid_sats: u32,
    #[prost(uint32, tag = "21")]
    pub solution_status: u32,
    #[prost(uint32, tag = "22")]
    pub solution_type: u32,
    #[prost(double, tag = "25")]
    pub gdop: f64,
    #[prost(double, tag = "26")]
    pub pdop: f64,
    #[prost(double, tag = "27")]
    pub hdop: f64,
    #[prost(double, tag = "28")]
    pub vdop: f64,
    #[prost(string, tag = "30")]
    pub utc_time: String,
    #[prost(double, tag = "31")]
    pub vel_e: f64,
    #[prost(double, tag = "32")]
    pub vel_n: f64,
    #[prost(double, tag = "33")]
    pub vel_u: f64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct NavMessage {
    #[prost(string, tag = "1")]
    pub system: String,
    #[prost(string, tag = "2")]
    pub signal: String,
    #[prost(int32, tag = "3")]
    pub prn: i32,
    #[prost(int32, tag = "4")]
    pub tow_at_current_symbol_ms: i32,
    #[prost(string, tag = "5")]
    pub nav_message: String,
}
