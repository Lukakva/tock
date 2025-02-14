// Licensed under the Apache License, Version 2.0 or the MIT License.
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright Tock Contributors 2022.

//! Implementation of the SAM4L ADCIFE.
//!
//! This is an implementation of the SAM4L analog to digital converter. It is
//! bare-bones because it provides little flexibility on how samples are taken.
//! Currently, all samples:
//!
//! - are 12 bits
//! - use the ground pad as the negative reference
//! - use a VCC/2 positive reference
//! - use a gain of 0.5x
//! - are left justified
//!
//! Samples can either be collected individually or continuously at a specified
//! frequency.
//!
//! - Author: Philip Levis <pal@cs.stanford.edu>, Branden Ghena <brghena@umich.edu>
//! - Updated: May 1, 2017

use crate::dma;
use crate::pm::{self, Clock, PBAClock};
use crate::scif;
use core::cell::Cell;
use core::{cmp, mem, slice};
use kernel::hil;
use kernel::utilities::cells::{OptionalCell, TakeCell};
use kernel::utilities::math;
use kernel::utilities::registers::interfaces::{Readable, Writeable};
use kernel::utilities::registers::{register_bitfields, ReadOnly, ReadWrite, WriteOnly};
use kernel::utilities::StaticRef;
use kernel::ErrorCode;

/// Representation of an ADC channel on the SAM4L.
#[derive(PartialEq)]
pub struct AdcChannel {
    chan_num: u32,
    internal: u32,
}

/// SAM4L ADC channels.
#[derive(Copy, Clone, Debug)]
#[repr(u8)]
pub enum Channel {
    AD0 = 0x00,
    AD1 = 0x01,
    AD2 = 0x02,
    AD3 = 0x03,
    AD4 = 0x04,
    AD5 = 0x05,
    AD6 = 0x06,
    AD7 = 0x07,
    AD8 = 0x08,
    AD9 = 0x09,
    AD10 = 0x0A,
    AD11 = 0x0B,
    AD12 = 0x0C,
    AD13 = 0x0D,
    AD14 = 0x0E,
    Bandgap = 0x0F,
    ScaledVCC = 0x12,
    DAC = 0x13,
    Vsingle = 0x16,
    ReferenceGround = 0x17,
}

/// Initialization of an ADC channel.
impl AdcChannel {
    /// Create a new ADC channel.
    ///
    /// - `channel`: Channel enum representing the channel number and whether it
    ///   is internal
    pub const fn new(channel: Channel) -> AdcChannel {
        AdcChannel {
            chan_num: ((channel as u8) & 0x0F) as u32,
            internal: (((channel as u8) >> 4) & 0x01) as u32,
        }
    }
}

/// ADC driver code for the SAM4L.
pub struct Adc {
    registers: StaticRef<AdcRegisters>,

    // state tracking for the ADC
    enabled: Cell<bool>,
    adc_clk_freq: Cell<u32>,
    active: Cell<bool>,
    continuous: Cell<bool>,
    dma_running: Cell<bool>,
    cpu_clock: Cell<bool>,

    // timer fire counting for slow sampling rates
    timer_repeats: Cell<u8>,
    timer_counts: Cell<u8>,

    // DMA peripheral, buffers, and length
    rx_dma: OptionalCell<&'static dma::DMAChannel>,
    rx_dma_peripheral: dma::DMAPeripheral,
    rx_length: Cell<usize>,
    next_dma_buffer: TakeCell<'static, [u16]>,
    next_dma_length: Cell<usize>,
    stopped_buffer: TakeCell<'static, [u16]>,

    // ADC client to send sample complete notifications to
    client: OptionalCell<&'static dyn hil::adc::Client>,
    highspeed_client: OptionalCell<&'static dyn hil::adc::HighSpeedClient>,
    pm: &'static pm::PowerManager,
}

/// Memory mapped registers for the ADC.
#[repr(C)]
pub struct AdcRegisters {
    // From page 1005 of SAM4L manual
    cr: WriteOnly<u32, Control::Register>,
    cfg: ReadWrite<u32, Configuration::Register>,
    sr: ReadOnly<u32, Status::Register>,
    scr: WriteOnly<u32, Interrupt::Register>,
    _reserved0: u32,
    seqcfg: ReadWrite<u32, SequencerConfig::Register>,
    cdma: WriteOnly<u32>,
    tim: ReadWrite<u32, TimingConfiguration::Register>,
    itimer: ReadWrite<u32, InternalTimer::Register>,
    wcfg: ReadWrite<u32, WindowMonitorConfiguration::Register>,
    wth: ReadWrite<u32, WindowMonitorThresholdConfiguration::Register>,
    lcv: ReadOnly<u32, SequencerLastConvertedValue::Register>,
    ier: WriteOnly<u32, Interrupt::Register>,
    idr: WriteOnly<u32, Interrupt::Register>,
    imr: ReadOnly<u32, Interrupt::Register>,
    calib: ReadWrite<u32>,
}

register_bitfields![u32,
    Control [
        /// Bandgap buffer request disable
        BGREQDIS 11,
        /// Bandgap buffer request enable
        BGREQEN 10,
        /// ADCIFE disable
        DIS 9,
        /// ADCIFE enable
        EN 8,
        /// Reference buffer disable
        REFBUFDIS 5,
        /// Reference buffer enable
        REFBUFEN 4,
        /// Sequence trigger
        STRIG 3,
        /// Internal timer start bit
        TSTART 2,
        /// Internal timer stop bit
        TSTOP 1,
        /// Software reset
        SWRST 0
    ],

    Configuration [
        /// Prescaler Rate Selection
        PRESCAL OFFSET(8) NUMBITS(3) [
            DIV4 = 0,
            DIV8 = 1,
            DIV16 = 2,
            DIV32 = 3,
            DIV64 = 4,
            DIV128 = 5,
            DIV256 = 6,
            DIV512 = 7
        ],
        /// Clock Selection for sequencer/ADC cell
        CLKSEL OFFSET(6) NUMBITS(1) [
            GenericClock = 0,
            ApbClock = 1
        ],
        /// ADC current reduction
        SPEED OFFSET(4) NUMBITS(2) [
            ksps300 = 0,
            ksps225 = 1,
            ksps150 = 2,
            ksps75 = 3
        ],
        /// ADC Reference selection
        REFSEL OFFSET(1) NUMBITS(3) [
            Internal1V = 0,
            VccX0p625 = 1,
            ExternalRef1 = 2,
            ExternalRef2 = 3,
            VccX0p5 = 4
        ]
    ],

    Status [
        /// Bandgap buffer request Status
        BGREQ 30,
        /// Reference Buffer Status
        REFBUF 28,
        /// Conversion busy
        CBUSY 27,
        /// Sequencer busy
        SBUSY 26,
        /// Timer busy
        TBUSY 25,
        /// Enable Status
        EN 24,
        /// Timer time out
        TTO 5,
        /// Sequencer missed trigger event
        SMTRG 3,
        /// Window monitor
        WM 2,
        /// Sequencer last converted value overrun
        LOVR 1,
        /// Sequencer end of conversion
        SEOC 0
    ],

    Interrupt [
        /// Timer time out
        TTO 5,
        /// Sequencer missed trigger event
        SMTRG 3,
        /// Window monitor
        WM 2,
        /// Sequencer last converted value overrun
        LOVR 1,
        /// Sequencer end of conversion
        SEOC 0
    ],

    SequencerConfig [
        /// Zoom shift/unipolar reference source selection
        ZOOMRANGE OFFSET(28) NUMBITS(3) [],
        /// MUX selection on Negative ADC input channel
        MUXNEG OFFSET(20) NUMBITS(3) [],
        /// MUS selection of Positive ADC input channel
        MUXPOS OFFSET(16) NUMBITS(4) [],
        /// Internal Voltage Sources Selection
        INTERNAL OFFSET(14) NUMBITS(2) [],
        /// Resolution
        RES OFFSET(12) NUMBITS(1) [
            Bits12 = 0,
            Bits8 = 1
        ],
        /// Trigger selection
        TRGSEL OFFSET(8) NUMBITS(3) [
            Software = 0,
            InternalAdcTimer = 1,
            InternalTriggerSource = 2,
            ContinuousMode = 3,
            ExternalTriggerPinRising = 4,
            ExternalTriggerPinFalling = 5,
            ExternalTriggerPinBoth = 6
        ],
        /// Gain Compensation
        GCOMP OFFSET(7) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],
        /// Gain factor
        GAIN OFFSET(4) NUMBITS(3) [
            Gain1x = 0,
            Gain2x = 1,
            Gain4x = 2,
            Gain8x = 3,
            Gain16x = 4,
            Gain32x = 5,
            Gain64x = 6,
            Gain0p5x = 7
        ],
        /// Bipolar Mode
        BIPOLAR OFFSET(2) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],
        /// Half Word Left Adjust
        HWLA OFFSET(0) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ]
    ],

    TimingConfiguration [
        /// Enable Startup
        ENSTUP OFFSET(8) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],
        /// Startup time
        STARTUP OFFSET(0) NUMBITS(5) []
    ],

    InternalTimer [
        /// Internal Timer Max Counter
        ITMC OFFSET(0) NUMBITS(16) []
    ],

    WindowMonitorConfiguration [
        /// Window Monitor Mode
        WM OFFSET(12) NUMBITS(3) []
    ],

    WindowMonitorThresholdConfiguration [
        /// High Threshold
        HT OFFSET(16) NUMBITS(12) [],
        /// Low Threshold
        LT OFFSET(0) NUMBITS(12) []
    ],

    SequencerLastConvertedValue [
        /// Last converted negative channel
        LCNC OFFSET(20) NUMBITS(3) [],
        /// Last converted positive channel
        LCPC OFFSET(16) NUMBITS(4) [],
        /// Last converted value
        LCV OFFSET(0) NUMBITS(16) []
    ]
];

// Page 59 of SAM4L data sheet
const BASE_ADDRESS: StaticRef<AdcRegisters> =
    unsafe { StaticRef::new(0x40038000 as *const AdcRegisters) };

/// Functions for initializing the ADC.
impl Adc {
    /// Create a new ADC driver.
    ///
    /// - `rx_dma_peripheral`: type used for DMA transactions
    pub fn new(rx_dma_peripheral: dma::DMAPeripheral, pm: &'static pm::PowerManager) -> Adc {
        Adc {
            // pointer to memory mapped I/O registers
            registers: BASE_ADDRESS,

            // status of the ADC peripheral
            enabled: Cell::new(false),
            adc_clk_freq: Cell::new(0),
            active: Cell::new(false),
            continuous: Cell::new(false),
            dma_running: Cell::new(false),
            cpu_clock: Cell::new(false),

            // timer repeating state for slow sampling rates
            timer_repeats: Cell::new(0),
            timer_counts: Cell::new(0),

            // DMA status and stuff
            rx_dma: OptionalCell::empty(),
            rx_dma_peripheral: rx_dma_peripheral,
            rx_length: Cell::new(0),
            next_dma_buffer: TakeCell::empty(),
            next_dma_length: Cell::new(0),
            stopped_buffer: TakeCell::empty(),

            // higher layer to send responses to
            client: OptionalCell::empty(),
            highspeed_client: OptionalCell::empty(),
            pm,
        }
    }

    /// Sets the DMA channel for this driver.
    ///
    /// - `rx_dma`: reference to the DMA channel the ADC should use
    pub fn set_dma(&self, rx_dma: &'static dma::DMAChannel) {
        self.rx_dma.set(rx_dma);
    }

    /// Interrupt handler for the ADC.
    pub fn handle_interrupt(&self) {
        let status = self.registers.sr.is_set(Status::SEOC);

        if self.enabled.get() && self.active.get() {
            if status {
                // sample complete interrupt

                // should we deal with this sample now, or wait for the next
                // one?
                if self.timer_counts.get() >= self.timer_repeats.get() {
                    // we actually care about this sample

                    // single sample complete. Send value to client
                    let val = self.registers.lcv.read(SequencerLastConvertedValue::LCV) as u16;
                    self.client.map(|client| {
                        client.sample_ready(val);
                    });

                    // clean up state
                    if self.continuous.get() {
                        // continuous sampling, reset counts and keep going
                        self.timer_counts.set(0);
                    } else {
                        // single sampling, disable interrupt and set inactive
                        self.active.set(false);
                        self.registers.idr.write(Interrupt::SEOC::SET);
                    }
                } else {
                    // increment count and wait for next sample
                    self.timer_counts.set(self.timer_counts.get() + 1);
                }

                // clear status
                self.registers.scr.write(Interrupt::SEOC::SET);
            }
        } else {
            // we are inactive, why did we get an interrupt?
            // disable all interrupts, clear status, and just ignore it
            self.registers.idr.write(
                Interrupt::TTO::SET
                    + Interrupt::SMTRG::SET
                    + Interrupt::WM::SET
                    + Interrupt::LOVR::SET
                    + Interrupt::SEOC::SET,
            );
            self.clear_status();
        }
    }

    /// Clear all status bits using the status clear register.
    fn clear_status(&self) {
        self.registers.scr.write(
            Interrupt::TTO::SET
                + Interrupt::SMTRG::SET
                + Interrupt::WM::SET
                + Interrupt::LOVR::SET
                + Interrupt::SEOC::SET,
        );
    }

    // Configures the ADC with the slowest clock that can provide continuous sampling at
    // the desired frequency and enables the ADC. Subsequent calls with the same frequency
    // value have no effect. Using the slowest clock also ensures efficient discrete
    // sampling.
    fn config_and_enable(&self, frequency: u32) -> Result<(), ErrorCode> {
        if self.active.get() {
            // disallow reconfiguration during sampling
            Err(ErrorCode::BUSY)
        } else if frequency == self.adc_clk_freq.get() {
            // already configured to work on this frequency
            Ok(())
        } else {
            // disabling the ADC before switching clocks is necessary to avoid
            // leaving it in undefined state
            self.registers.cr.write(Control::DIS::SET);

            // wait until status is disabled
            let mut timeout = 10000;
            while self.registers.sr.is_set(Status::EN) {
                timeout -= 1;
                if timeout == 0 {
                    // ADC never disabled
                    return Err(ErrorCode::FAIL);
                }
            }

            self.enabled.set(true);

            // configure the ADC max speed and reference select
            let mut cfg_val = Configuration::SPEED::ksps300 + Configuration::REFSEL::VccX0p5;

            // First, enable the clocks
            // Both the ADCIFE clock and GCLK10 are needed,
            // but the GCLK10 source depends on the requested sampling frequency

            // turn on ADCIFE bus clock. Already set to the same frequency
            // as the CPU clock
            pm::enable_clock(Clock::PBA(PBAClock::ADCIFE));

            // Now, determine the prescalar.
            // The maximum sampling frequency with the RC clocks is 1/32th of their clock
            // frequency. This is because of the minimum PRESCAL by a factor of 4 and the
            // 7+1 cycles needed for conversion in continuous mode. Hence, 4*(7+1)=32.
            if frequency <= 113600 / 32 {
                // RC oscillator
                self.cpu_clock.set(false);
                let max_freq: u32;
                if frequency <= 32000 / 32 {
                    // frequency of the RC32K is 32KHz.
                    scif::generic_clock_enable(
                        scif::GenericClock::GCLK10,
                        scif::ClockSource::RC32K,
                    );
                    max_freq = 32000 / 32;
                } else {
                    // frequency of the RCSYS is 115KHz.
                    scif::generic_clock_enable(
                        scif::GenericClock::GCLK10,
                        scif::ClockSource::RCSYS,
                    );
                    max_freq = 113600 / 32;
                }
                let divisor = (frequency + max_freq - 1) / frequency; // ceiling of division
                let divisor_pow2 = math::closest_power_of_two(divisor);
                let clock_divisor = cmp::min(math::log_base_two(divisor_pow2), 7);
                self.adc_clk_freq.set(max_freq / (1 << (clock_divisor)));
                cfg_val += Configuration::PRESCAL.val(clock_divisor);
            } else {
                // CPU clock
                self.cpu_clock.set(true);
                scif::generic_clock_enable(scif::GenericClock::GCLK10, scif::ClockSource::CLK_CPU);
                // determine clock divider
                // we need the ADC_CLK to be a maximum of 1.5 MHz in frequency,
                // so we need to find the PRESCAL value that will make this
                // happen.
                // Formula: f(ADC_CLK) = f(CLK_CPU)/2^(N+2) <= 1.5 MHz
                // and we solve for N
                // becomes: N <= ceil(log_2(f(CLK_CPU)/1500000)) - 2
                let cpu_frequency = self.pm.get_system_frequency();
                let divisor = (cpu_frequency + (1500000 - 1)) / 1500000; // ceiling of division
                let divisor_pow2 = math::closest_power_of_two(divisor);
                let clock_divisor = cmp::min(
                    math::log_base_two(divisor_pow2).checked_sub(2).unwrap_or(0),
                    7,
                );
                self.adc_clk_freq
                    .set(cpu_frequency / (1 << (clock_divisor + 2)));
                cfg_val += Configuration::PRESCAL.val(clock_divisor);
            }

            if self.cpu_clock.get() {
                cfg_val += Configuration::CLKSEL::ApbClock
            } else {
                cfg_val += Configuration::CLKSEL::GenericClock
            }

            self.registers.cfg.write(cfg_val);

            // set startup to wait 24 cycles
            self.registers.tim.write(
                TimingConfiguration::ENSTUP::Enable + TimingConfiguration::STARTUP.val(0x17),
            );

            // software reset (does not clear registers)
            self.registers.cr.write(Control::SWRST::SET);

            // enable ADC
            self.registers.cr.write(Control::EN::SET);

            // wait until status is enabled
            let mut timeout = 10000;
            while !self.registers.sr.is_set(Status::EN) {
                timeout -= 1;
                if timeout == 0 {
                    // ADC never enabled
                    return Err(ErrorCode::FAIL);
                }
            }

            // enable Bandgap buffer and Reference buffer. I don't actually
            // know what these do, but you need to turn them on
            self.registers
                .cr
                .write(Control::BGREQEN::SET + Control::REFBUFEN::SET);

            // wait until buffers are enabled
            timeout = 100000;
            while !self
                .registers
                .sr
                .matches_all(Status::BGREQ::SET + Status::REFBUF::SET + Status::EN::SET)
            {
                timeout -= 1;
                if timeout == 0 {
                    // ADC buffers never enabled
                    return Err(ErrorCode::FAIL);
                }
            }

            Ok(())
        }
    }

    /// Disables the ADC so that the chip can return to deep sleep
    fn disable(&self) {
        // disable ADC
        self.registers.cr.write(Control::DIS::SET);

        // wait until status is disabled
        let mut timeout = 10000;
        while self.registers.sr.is_set(Status::EN) {
            timeout -= 1;
            if timeout == 0 {
                // ADC never disabled
                return;
            }
        }

        // disable bandgap and reference buffers
        self.registers
            .cr
            .write(Control::BGREQDIS::SET + Control::REFBUFDIS::SET);

        self.enabled.set(false);
        scif::generic_clock_disable(scif::GenericClock::GCLK10);
        pm::disable_clock(Clock::PBA(PBAClock::ADCIFE));
    }
}

/// Implements an ADC capable reading ADC samples on any channel.
impl hil::adc::Adc for Adc {
    type Channel = AdcChannel;

    /// Capture a single analog sample, calling the client when complete.
    /// Returns an error if the ADC is already sampling.
    ///
    /// - `channel`: the ADC channel to sample
    fn sample(&self, channel: &Self::Channel) -> Result<(), ErrorCode> {
        // always configure to 1KHz to get the slowest clock with single sampling
        let res = self.config_and_enable(1000);

        if res != Ok(()) {
            res
        } else if !self.enabled.get() {
            Err(ErrorCode::OFF)
        } else if self.active.get() {
            // only one operation at a time
            Err(ErrorCode::BUSY)
        } else {
            self.active.set(true);
            self.continuous.set(false);
            self.timer_repeats.set(0);
            self.timer_counts.set(0);

            let cfg = SequencerConfig::MUXNEG.val(0x7) + // ground pad
                SequencerConfig::MUXPOS.val(channel.chan_num)
                + SequencerConfig::INTERNAL.val(0x2 | channel.internal)
                + SequencerConfig::RES::Bits12
                + SequencerConfig::TRGSEL::Software
                + SequencerConfig::GCOMP::Disable
                + SequencerConfig::GAIN::Gain0p5x
                + SequencerConfig::BIPOLAR::Disable
                + SequencerConfig::HWLA::Enable;
            self.registers.seqcfg.write(cfg);

            // clear any current status
            self.clear_status();

            // enable end of conversion interrupt
            self.registers.ier.write(Interrupt::SEOC::SET);

            // initiate conversion
            self.registers.cr.write(Control::STRIG::SET);

            Ok(())
        }
    }

    /// Request repeated analog samples on a particular channel, calling after
    /// each sample. In order to not unacceptably slow down the system
    /// collecting samples, this interface is limited to one sample every 100
    /// microseconds (10000 samples per second). To sample faster, use the
    /// sample_highspeed function.
    ///
    /// - `channel`: the ADC channel to sample
    /// - `frequency`: the number of samples per second to collect
    fn sample_continuous(&self, channel: &Self::Channel, frequency: u32) -> Result<(), ErrorCode> {
        let res = self.config_and_enable(frequency);

        if res != Ok(()) {
            res
        } else if !self.enabled.get() {
            Err(ErrorCode::OFF)
        } else if self.active.get() {
            // only one sample at a time
            Err(ErrorCode::BUSY)
        } else if frequency == 0 || frequency > 10000 {
            // limit sampling frequencies to a valid range
            Err(ErrorCode::INVAL)
        } else {
            self.active.set(true);
            self.continuous.set(true);

            // adc sequencer configuration
            let mut cfg = SequencerConfig::MUXNEG.val(0x7) + // ground pad
                SequencerConfig::MUXPOS.val(channel.chan_num)
                + SequencerConfig::INTERNAL.val(0x2 | channel.internal)
                + SequencerConfig::RES::Bits12
                + SequencerConfig::GCOMP::Disable
                + SequencerConfig::GAIN::Gain0p5x
                + SequencerConfig::BIPOLAR::Disable
                + SequencerConfig::HWLA::Enable;
            // set trigger based on how good our clock is
            if self.cpu_clock.get() {
                cfg += SequencerConfig::TRGSEL::InternalAdcTimer;
            } else {
                cfg += SequencerConfig::TRGSEL::ContinuousMode;
            }
            self.registers.seqcfg.write(cfg);

            // stop timer if running
            self.registers.cr.write(Control::TSTOP::SET);

            if self.cpu_clock.get() {
                // This logic only applies for sampling off the CPU
                // setup timer for low-frequency samples. Based on the ADC clock,
                // the minimum timer frequency is:
                // 1500000 / (0xFFFF + 1) = 22.888 Hz.
                // So for any frequency less than 23 Hz, we will keep our own
                // counter in addition and only actually perform a callback every N
                // timer fires. This is important to enable low-jitter sampling in
                // the 1-22 Hz range.
                let timer_frequency;
                if frequency < 23 {
                    // set a number of timer repeats before the callback is
                    // performed. 60 here is an arbitrary number which limits the
                    // actual itimer frequency to between 42 and 60 in the desired
                    // range of 1-22 Hz, which seems slow enough to keep the system
                    // from getting bogged down with interrupts
                    let counts = 60 / frequency;
                    self.timer_repeats.set(counts as u8);
                    self.timer_counts.set(0);
                    timer_frequency = frequency * counts;
                } else {
                    // we can sample at this frequency directly with the timer
                    self.timer_repeats.set(0);
                    self.timer_counts.set(0);
                    timer_frequency = frequency;
                }

                // set timer, limit to bounds
                // f(timer) = f(adc) / (counter + 1)
                let mut counter = (self.adc_clk_freq.get() / timer_frequency) - 1;
                counter = cmp::max(cmp::min(counter, 0xFFFF), 0);
                self.registers
                    .itimer
                    .write(InternalTimer::ITMC.val(counter));
            } else {
                // we can sample at this frequency directly with the timer
                self.timer_repeats.set(0);
                self.timer_counts.set(0);
            }

            // clear any current status
            self.clear_status();

            // enable end of conversion interrupt
            self.registers.ier.write(Interrupt::SEOC::SET);

            // start timer
            self.registers.cr.write(Control::TSTART::SET);

            Ok(())
        }
    }

    /// Stop continuously sampling the ADC.
    /// This is expected to be called to stop continuous sampling operations,
    /// but can be called to abort any currently running operation. The buffer,
    /// if any, will be returned via the `samples_ready` callback.
    fn stop_sampling(&self) -> Result<(), ErrorCode> {
        if !self.enabled.get() {
            Err(ErrorCode::OFF)
        } else if !self.active.get() {
            // cannot cancel sampling that isn't running
            Err(ErrorCode::INVAL)
        } else {
            // clean up state
            self.active.set(false);
            self.continuous.set(false);
            self.dma_running.set(false);

            // stop internal timer
            self.registers.cr.write(Control::TSTOP::SET);

            // disable sample interrupts
            self.registers.idr.write(Interrupt::SEOC::SET);

            // reset the ADC peripheral
            self.registers.cr.write(Control::SWRST::SET);

            // disable the ADC
            self.disable();

            // stop DMA transfer if going. This should safely return a None if
            // the DMA was not being used
            let dma_buffer = self.rx_dma.map_or(None, |rx_dma| {
                let dma_buf = rx_dma.abort_transfer();
                rx_dma.disable();
                dma_buf
            });
            self.rx_length.set(0);

            // store the buffer if it exists
            dma_buffer.map(|dma_buf| {
                // change buffer back into a [u16]
                // the buffer was originally a [u16] so this should be okay
                let buf_ptr = unsafe { mem::transmute::<*mut u8, *mut u16>(dma_buf.as_mut_ptr()) };
                let buf = unsafe { slice::from_raw_parts_mut(buf_ptr, dma_buf.len() / 2) };

                // we'll place it here so we can return it to the higher level
                // later in a `retrieve_buffers` call
                self.stopped_buffer.replace(buf);
            });

            Ok(())
        }
    }

    /// Resolution of the reading.
    fn get_resolution_bits(&self) -> usize {
        12
    }

    /// Voltage reference is VCC/2, we assume VCC is 3.3 V, and we use a gain
    /// of 0.5.
    fn get_voltage_reference_mv(&self) -> Option<usize> {
        Some(3300)
    }

    /// Sets the client for this driver.
    ///
    /// - `client`: reference to capsule which handles responses
    fn set_client(&self, client: &'static dyn hil::adc::Client) {
        self.client.set(client);
    }
}

/// Implements an ADC capable of continuous sampling
impl hil::adc::AdcHighSpeed for Adc {
    /// Capture buffered samples from the ADC continuously at a given
    /// frequency, calling the client whenever a buffer fills up. The client is
    /// then expected to either stop sampling or provide an additional buffer
    /// to sample into. Note that due to hardware constraints the maximum
    /// frequency range of the ADC is from 187 kHz to 23 Hz (although its
    /// precision is limited at higher frequencies due to aliasing).
    ///
    /// - `channel`: the ADC channel to sample
    /// - `frequency`: frequency to sample at
    /// - `buffer1`: first buffer to fill with samples
    /// - `length1`: number of samples to collect (up to buffer length)
    /// - `buffer2`: second buffer to fill once the first is full
    /// - `length2`: number of samples to collect (up to buffer length)
    fn sample_highspeed(
        &self,
        channel: &Self::Channel,
        frequency: u32,
        buffer1: &'static mut [u16],
        length1: usize,
        buffer2: &'static mut [u16],
        length2: usize,
    ) -> Result<(), (ErrorCode, &'static mut [u16], &'static mut [u16])> {
        let res = self.config_and_enable(frequency);

        if res != Ok(()) {
            Err((res.unwrap_err(), buffer1, buffer2))
        } else if !self.enabled.get() {
            Err((ErrorCode::OFF, buffer1, buffer2))
        } else if self.active.get() {
            // only one sample at a time
            Err((ErrorCode::BUSY, buffer1, buffer2))
        } else if frequency <= (self.adc_clk_freq.get() / (0xFFFF + 1)) || frequency > 250000 {
            // can't sample faster than the max sampling frequency or slower
            // than the timer can be set to
            Err((ErrorCode::INVAL, buffer1, buffer2))
        } else if length1 == 0 {
            // at least need a valid length for the for the first buffer full of
            // samples. Otherwise, what are we doing here?
            Err((ErrorCode::INVAL, buffer1, buffer2))
        } else {
            self.active.set(true);
            self.continuous.set(true);

            // store the second buffer for later use
            self.next_dma_buffer.replace(buffer2);
            self.next_dma_length.set(length2);

            // adc sequencer configuration
            let mut cfg = SequencerConfig::MUXNEG.val(0x7) + // ground pad
                SequencerConfig::MUXPOS.val(channel.chan_num)
                + SequencerConfig::INTERNAL.val(0x2 | channel.internal)
                + SequencerConfig::RES::Bits12
                + SequencerConfig::GCOMP::Disable
                + SequencerConfig::GAIN::Gain0p5x
                + SequencerConfig::BIPOLAR::Disable
                + SequencerConfig::HWLA::Enable;
            // set trigger based on how good our clock is
            if self.cpu_clock.get() {
                cfg += SequencerConfig::TRGSEL::InternalAdcTimer;
            } else {
                cfg += SequencerConfig::TRGSEL::ContinuousMode;
            }
            self.registers.seqcfg.write(cfg);

            // stop timer if running
            self.registers.cr.write(Control::TSTOP::SET);

            if self.cpu_clock.get() {
                // set timer, limit to bounds
                // f(timer) = f(adc) / (counter + 1)
                let mut counter = (self.adc_clk_freq.get() / frequency) - 1;
                counter = cmp::max(cmp::min(counter, 0xFFFF), 0);
                self.registers
                    .itimer
                    .write(InternalTimer::ITMC.val(counter));
            }

            // clear any current status
            self.clear_status();

            // receive up to the buffer's length samples
            let dma_len = cmp::min(buffer1.len(), length1);

            // change buffer into a [u8]
            // this is unsafe but acceptable for the following reasons
            //  * the buffer is aligned based on 16-bit boundary, so the 8-bit
            //    alignment is fine
            //  * the DMA is doing checking based on our expected data width to
            //    make sure we don't go past dma_buf.len()/width
            //  * we will transmute the array back to a [u16] after the DMA
            //    transfer is complete
            let dma_buf_ptr = unsafe { mem::transmute::<*mut u16, *mut u8>(buffer1.as_mut_ptr()) };
            let dma_buf = unsafe { slice::from_raw_parts_mut(dma_buf_ptr, buffer1.len() * 2) };

            // set up the DMA
            self.rx_dma.map(move |dma| {
                self.dma_running.set(true);
                dma.enable();
                self.rx_length.set(dma_len);
                dma.do_transfer(self.rx_dma_peripheral, dma_buf, dma_len);
            });

            // start timer
            self.registers.cr.write(Control::TSTART::SET);

            Ok(())
        }
    }

    /// Provide a new buffer to send on-going buffered continuous samples to.
    /// This is expected to be called after the `samples_ready` callback.
    ///
    /// - `buf`: buffer to fill with samples
    /// - `length`: number of samples to collect (up to buffer length)
    fn provide_buffer(
        &self,
        buf: &'static mut [u16],
        length: usize,
    ) -> Result<(), (ErrorCode, &'static mut [u16])> {
        if !self.enabled.get() {
            Err((ErrorCode::OFF, buf))
        } else if !self.active.get() {
            // cannot continue sampling that isn't running
            Err((ErrorCode::INVAL, buf))
        } else if !self.continuous.get() {
            // cannot continue a single sample operation
            Err((ErrorCode::INVAL, buf))
        } else if self.next_dma_buffer.is_some() {
            // we've already got a second buffer, we don't need a third yet
            Err((ErrorCode::BUSY, buf))
        } else {
            // store the buffer for later use
            self.next_dma_buffer.replace(buf);
            self.next_dma_length.set(length);

            Ok(())
        }
    }

    /// Reclaim buffers after the ADC is stopped.
    /// This is expected to be called after `stop_sampling`.
    fn retrieve_buffers(
        &self,
    ) -> Result<(Option<&'static mut [u16]>, Option<&'static mut [u16]>), ErrorCode> {
        if self.active.get() {
            // cannot return buffers while running
            Err(ErrorCode::INVAL)
        } else {
            // we're not running, so give back whatever we've got
            Ok((self.next_dma_buffer.take(), self.stopped_buffer.take()))
        }
    }

    fn set_highspeed_client(&self, client: &'static dyn hil::adc::HighSpeedClient) {
        self.highspeed_client.set(client);
    }
}

/// Implements a client of a DMA.
impl dma::DMAClient for Adc {
    /// Handler for DMA transfer completion.
    ///
    /// - `pid`: the DMA peripheral that is complete
    fn transfer_done(&self, pid: dma::DMAPeripheral) {
        // check if this was an RX transfer
        if pid == self.rx_dma_peripheral {
            // RX transfer was completed

            // get buffer filled with samples from DMA
            let dma_buffer = self.rx_dma.map_or(None, |rx_dma| {
                self.dma_running.set(false);
                let dma_buf = rx_dma.abort_transfer();
                rx_dma.disable();
                dma_buf
            });

            // get length of received buffer
            let length = self.rx_length.get();

            // start a new transfer with the next buffer
            // we need to do this quickly in order to keep from missing samples.
            // At 175000 Hz, we only have 5.8 us (~274 cycles) to do so
            self.next_dma_buffer.take().map(|buf| {
                // first determine the buffer's length in samples
                let dma_len = cmp::min(buf.len(), self.next_dma_length.get());

                // only continue with a nonzero length. If we were given a
                // zero-length buffer or length field, assume that the user knew
                // what was going on, and just don't use the buffer
                if dma_len > 0 {
                    // change buffer into a [u8]
                    // this is unsafe but acceptable for the following reasons
                    //  * the buffer is aligned based on 16-bit boundary, so the
                    //    8-bit alignment is fine
                    //  * the DMA is doing checking based on our expected data
                    //    width to make sure we don't go past
                    //    dma_buf.len()/width
                    //  * we will transmute the array back to a [u16] after the
                    //    DMA transfer is complete
                    let dma_buf_ptr =
                        unsafe { mem::transmute::<*mut u16, *mut u8>(buf.as_mut_ptr()) };
                    let dma_buf = unsafe { slice::from_raw_parts_mut(dma_buf_ptr, buf.len() * 2) };

                    // set up the DMA
                    self.rx_dma.map(move |dma| {
                        self.dma_running.set(true);
                        dma.enable();
                        self.rx_length.set(dma_len);
                        dma.do_transfer(self.rx_dma_peripheral, dma_buf, dma_len);
                    });
                } else {
                    // if length was zero, just keep the buffer in the takecell
                    // so we can return it when `stop_sampling` is called
                    self.next_dma_buffer.replace(buf);
                }
            });

            // alert client
            self.highspeed_client.map(|client| {
                dma_buffer.map(|dma_buf| {
                    // change buffer back into a [u16]
                    // the buffer was originally a [u16] so this should be okay
                    let buf_ptr =
                        unsafe { mem::transmute::<*mut u8, *mut u16>(dma_buf.as_mut_ptr()) };
                    let buf = unsafe { slice::from_raw_parts_mut(buf_ptr, dma_buf.len() / 2) };

                    // pass the buffer up to the next layer. It will then either
                    // send down another buffer to continue sampling, or stop
                    // sampling
                    client.samples_ready(buf, length);
                });
            });
        }
    }
}
