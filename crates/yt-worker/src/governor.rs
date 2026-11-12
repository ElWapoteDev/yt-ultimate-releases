//! The power and temperature policy of the worker (`docs/AI_DJ.md` §5.9):
//! from what the operating system reports, whether work may run now and what
//! share of the time it may keep the GPU or a CPU core busy. Pure: the worker
//! samples the conditions and applies the verdict.

use crate::protocol::DeferReason;
use std::time::Duration;

/// Share of the time work may be busy on mains power at a normal temperature.
pub const AC_DUTY_CYCLE: f64 = 0.5;
/// Share of the time work may be busy on battery, with a warm device or when
/// the power source is unknown.
pub const REDUCED_DUTY_CYCLE: f64 = 0.25;

/// What powers the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerSource {
    Ac,
    Battery,
    Unknown,
}

/// The device's thermal state, as macOS reports it. Windows has no public
/// equivalent and reports `Nominal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Thermal {
    Nominal,
    /// Warm: work goes on at a reduced duty cycle.
    Fair,
    /// Hot: work pauses.
    Serious,
    /// Throttling to protect the hardware: work pauses.
    Critical,
}

/// What the operating system reports about power and temperature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Conditions {
    pub power: PowerSource,
    /// The battery is nearly empty (below about 20 %).
    pub battery_low: bool,
    /// Low Power Mode (macOS) or battery saver (Windows) is on.
    pub low_power: bool,
    pub thermal: Thermal,
}

/// Whether work may run under some [`Conditions`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Verdict {
    /// Run, keeping the device busy at most `duty_cycle` (0–1) of the time.
    Run { duty_cycle: f64 },
    /// Hold work back.
    Defer(DeferReason),
}

/// The policy of `docs/AI_DJ.md` §5.9 for `conditions`.
pub fn verdict(conditions: Conditions) -> Verdict {
    if conditions.thermal >= Thermal::Serious {
        return Verdict::Defer(DeferReason::Thermal);
    }
    if conditions.low_power || conditions.battery_low {
        return Verdict::Defer(DeferReason::LowPower);
    }
    let full = conditions.power == PowerSource::Ac && conditions.thermal == Thermal::Nominal;
    Verdict::Run {
        duty_cycle: if full {
            AC_DUTY_CYCLE
        } else {
            REDUCED_DUTY_CYCLE
        },
    }
}

/// How long to stay idle after `busy` so the busy share of the time stays at
/// `duty_cycle`.
pub fn rest_after(busy: Duration, duty_cycle: f64) -> Duration {
    let duty_cycle = duty_cycle.clamp(REDUCED_DUTY_CYCLE / 4.0, 1.0);
    busy.mul_f64((1.0 - duty_cycle) / duty_cycle)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CALM: Conditions = Conditions {
        power: PowerSource::Ac,
        battery_low: false,
        low_power: false,
        thermal: Thermal::Nominal,
    };

    #[test]
    fn mains_power_at_a_normal_temperature_runs_at_half_the_time() {
        assert_eq!(
            verdict(CALM),
            Verdict::Run {
                duty_cycle: AC_DUTY_CYCLE
            }
        );
    }

    #[test]
    fn warmth_battery_and_unknown_power_reduce_the_duty_cycle() {
        let reduced = Verdict::Run {
            duty_cycle: REDUCED_DUTY_CYCLE,
        };
        for conditions in [
            Conditions {
                thermal: Thermal::Fair,
                ..CALM
            },
            Conditions {
                power: PowerSource::Battery,
                ..CALM
            },
            Conditions {
                power: PowerSource::Unknown,
                ..CALM
            },
        ] {
            assert_eq!(verdict(conditions), reduced, "{conditions:?}");
        }
    }

    #[test]
    fn heat_and_power_saving_hold_work_back() {
        for thermal in [Thermal::Serious, Thermal::Critical] {
            assert_eq!(
                verdict(Conditions { thermal, ..CALM }),
                Verdict::Defer(DeferReason::Thermal)
            );
        }
        for conditions in [
            Conditions {
                low_power: true,
                ..CALM
            },
            Conditions {
                power: PowerSource::Battery,
                battery_low: true,
                ..CALM
            },
        ] {
            assert_eq!(verdict(conditions), Verdict::Defer(DeferReason::LowPower));
        }
        assert_eq!(
            verdict(Conditions {
                thermal: Thermal::Critical,
                low_power: true,
                ..CALM
            }),
            Verdict::Defer(DeferReason::Thermal),
            "heat wins: it is the more urgent reason"
        );
    }

    #[test]
    fn rest_keeps_the_busy_share_at_the_duty_cycle() {
        let busy = Duration::from_millis(40);
        assert_eq!(rest_after(busy, 0.5), busy);
        assert_eq!(rest_after(busy, 0.25), Duration::from_millis(120));
        assert_eq!(rest_after(busy, 1.0), Duration::ZERO);
        assert!(
            rest_after(busy, 0.0) <= busy * 15,
            "a nonsensical cycle never stalls work forever"
        );
    }
}
