//! What the operating system reports about power and temperature, and the
//! priority the worker runs at (`docs/AI_DJ.md` §5.9).

pub(crate) use imp::{conditions, lower_process_priority, set_thread_priority};

#[cfg(target_os = "macos")]
mod imp {
    use objc2_foundation::{NSProcessInfo, NSProcessInfoThermalState};
    use std::ffi::{c_char, c_void, CStr};
    use yt_worker::governor::{Conditions, PowerSource, Thermal};

    type CFTypeRef = *const c_void;
    /// `kCFStringEncodingUTF8`.
    const UTF8: u32 = 0x0800_0100;
    /// `kIOPSLowBatteryWarningEarly`: the battery is below about 22 %.
    const LOW_BATTERY_WARNING: i32 = 2;

    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOPSCopyPowerSourcesInfo() -> CFTypeRef;
        fn IOPSGetProvidingPowerSourceType(snapshot: CFTypeRef) -> CFTypeRef;
        fn IOPSGetBatteryWarningLevel() -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(value: CFTypeRef);
        fn CFStringGetCString(
            string: CFTypeRef,
            buffer: *mut c_char,
            size: isize,
            encoding: u32,
        ) -> u8;
    }

    /// `ProcessInfo.thermalState`, Low Power Mode and the power source.
    pub fn conditions() -> Conditions {
        let info = NSProcessInfo::processInfo();
        let state = info.thermalState();
        let thermal = if state == NSProcessInfoThermalState::Nominal {
            Thermal::Nominal
        } else if state == NSProcessInfoThermalState::Fair {
            Thermal::Fair
        } else if state == NSProcessInfoThermalState::Serious {
            Thermal::Serious
        } else {
            Thermal::Critical
        };
        let power = power_source();
        // SAFETY: a query without arguments that only returns a level.
        let warning = unsafe { IOPSGetBatteryWarningLevel() };
        Conditions {
            power,
            battery_low: power == PowerSource::Battery && warning >= LOW_BATTERY_WARNING,
            low_power: info.isLowPowerModeEnabled(),
            thermal,
        }
    }

    fn power_source() -> PowerSource {
        let mut name = [0_u8; 32];
        // SAFETY: the snapshot is owned here and released once; the source
        // type is a string it owns, copied into `name` before the release.
        let copied = unsafe {
            let snapshot = IOPSCopyPowerSourcesInfo();
            if snapshot.is_null() {
                return PowerSource::Unknown;
            }
            let kind = IOPSGetProvidingPowerSourceType(snapshot);
            let copied = !kind.is_null()
                && CFStringGetCString(
                    kind,
                    name.as_mut_ptr().cast::<c_char>(),
                    name.len() as isize,
                    UTF8,
                ) != 0;
            CFRelease(snapshot);
            copied
        };
        match CStr::from_bytes_until_nul(&name) {
            Ok(kind) if copied && kind.to_bytes() == b"AC Power" => PowerSource::Ac,
            Ok(kind) if copied && kind.to_bytes() == b"Battery Power" => PowerSource::Battery,
            _ => PowerSource::Unknown,
        }
    }

    /// macOS lowers each job's thread instead (`set_thread_priority`).
    pub fn lower_process_priority() {}

    /// The *utility* QoS class on mains power and *background* otherwise, so
    /// the work runs on efficiency cores at low priority; CoreML's own queues
    /// inherit it from the calling thread.
    pub fn set_thread_priority(power: PowerSource) {
        let class = if power == PowerSource::Ac {
            libc::qos_class_t::QOS_CLASS_UTILITY
        } else {
            libc::qos_class_t::QOS_CLASS_BACKGROUND
        };
        // SAFETY: changes only the calling thread's own QoS class.
        unsafe {
            libc::pthread_set_qos_class_self_np(class, 0);
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use windows_sys::Win32::System::{
        Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS},
        Threading::{
            GetCurrentProcess, ProcessPowerThrottling, SetPriorityClass, SetProcessInformation,
            BELOW_NORMAL_PRIORITY_CLASS, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE,
        },
    };
    use yt_worker::governor::{Conditions, PowerSource, Thermal};

    /// Battery charge below which the battery counts as nearly empty, in percent.
    const LOW_BATTERY_PERCENT: u8 = 20;
    /// `SYSTEM_POWER_STATUS::ACLineStatus` values.
    const AC_OFFLINE: u8 = 0;
    const AC_ONLINE: u8 = 1;
    /// `SYSTEM_POWER_STATUS::SystemStatusFlag` while battery saver is on.
    const BATTERY_SAVER_ON: u8 = 1;

    /// The power source, battery charge and battery saver. Windows has no
    /// public thermal state, so it reads as nominal.
    pub fn conditions() -> Conditions {
        let mut status = SYSTEM_POWER_STATUS {
            ACLineStatus: u8::MAX,
            BatteryFlag: u8::MAX,
            BatteryLifePercent: u8::MAX,
            SystemStatusFlag: 0,
            BatteryLifeTime: 0,
            BatteryFullLifeTime: 0,
        };
        // SAFETY: `status` is a live, writable struct of the layout the call fills.
        if unsafe { GetSystemPowerStatus(&mut status) } == 0 {
            return Conditions {
                power: PowerSource::Unknown,
                battery_low: false,
                low_power: false,
                thermal: Thermal::Nominal,
            };
        }
        let power = match status.ACLineStatus {
            AC_ONLINE => PowerSource::Ac,
            AC_OFFLINE => PowerSource::Battery,
            _ => PowerSource::Unknown,
        };
        Conditions {
            power,
            battery_low: power == PowerSource::Battery
                && status.BatteryLifePercent < LOW_BATTERY_PERCENT,
            low_power: status.SystemStatusFlag == BATTERY_SAVER_ON,
            thermal: Thermal::Nominal,
        }
    }

    /// EcoQoS for the whole process (efficient cores at low frequency) and a
    /// below-normal priority class. A failure leaves the default priority.
    pub fn lower_process_priority() {
        let throttling = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            StateMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        };
        // SAFETY: the pseudo-handle of this process, and a live struct of the
        // size passed.
        unsafe {
            let process = GetCurrentProcess();
            SetProcessInformation(
                process,
                ProcessPowerThrottling,
                std::ptr::from_ref(&throttling).cast::<c_void>(),
                size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
            );
            SetPriorityClass(process, BELOW_NORMAL_PRIORITY_CLASS);
        }
    }

    /// Windows lowers the whole process instead (`lower_process_priority`).
    pub fn set_thread_priority(_power: PowerSource) {}
}

#[cfg(not(any(target_os = "macos", windows)))]
mod imp {
    use yt_worker::governor::{Conditions, PowerSource, Thermal};

    /// Unknown power at a nominal temperature: the reduced duty cycle.
    pub fn conditions() -> Conditions {
        Conditions {
            power: PowerSource::Unknown,
            battery_low: false,
            low_power: false,
            thermal: Thermal::Nominal,
        }
    }

    pub fn lower_process_priority() {}

    pub fn set_thread_priority(_power: PowerSource) {}
}
