// SPDX-License-Identifier: Apache-2.0

use crate::backend::probe::x86_64::{CpuId, Vendor};
use crate::backend::sgx::{sgx_cache_dir, TcbPackage, AESM_SOCKET, FMSPC_PATH, TCB_PATH};
use crate::backend::Datum;
use crate::caching::CrlList;
use sgx::parameters::{Features, MiscSelect, Xfrm};

use std::arch::x86_64::__cpuid_count;
use std::fs::File;
use std::path::Path;
use std::time::SystemTime;

use chrono::{DateTime, Local};
use der::Decode;
use serde_json::Value;

fn humanize(mut size: f64) -> (f64, &'static str) {
    let mut iter = 0;

    while size > 512.0 {
        size /= 1024.0;
        iter += 1;
    }

    let suffix = match iter {
        0 => "",
        1 => "KiB",
        2 => "MiB",
        3 => "GiB",
        4 => "TiB",
        5 => "PiB",
        6 => "EiB",
        7 => "ZiB",
        8 => "YiB",
        _ => panic!("Size unsupported!"),
    };

    (size, suffix)
}

pub const CPUIDS: &[CpuId] = &[CpuId {
    name: "CPU",
    leaf: 0x80000000,
    subl: 0x00000000,
    func: |res| CpuId::cpu_identifier(res, Some(Vendor::Intel)),
    vend: None,
    data: &[CpuId {
        name: "SGX Support",
        leaf: 0x00000007,
        subl: 0x00000000,
        func: |res| (res.ebx & (1 << 2) != 0, None),
        vend: Some(Vendor::Intel),
        data: &[
            CpuId {
                name: "Version 1",
                leaf: 0x00000012,
                subl: 0x00000000,
                func: |res| (res.eax & (1 << 0) != 0, None),
                vend: Some(Vendor::Intel),
                data: &[],
            },
            CpuId {
                name: "Version 2",
                leaf: 0x00000012,
                subl: 0x00000000,
                func: |res| (res.eax & (1 << 1) != 0, None),
                vend: Some(Vendor::Intel),
                data: &[],
            },
            CpuId {
                name: "FLC Support",
                leaf: 0x00000007,
                subl: 0x00000000,
                func: |res| (res.ecx & (1 << 30) != 0, None),
                vend: Some(Vendor::Intel),
                data: &[],
            },
            CpuId {
                name: "Max Size (32-bit)",
                leaf: 0x00000012,
                subl: 0x00000000,
                func: |res| {
                    let bits = res.edx as u8;
                    let (n, s) = humanize((1u64 << bits) as f64);
                    (true, Some(format!("{n:.0} {s}")))
                },
                vend: Some(Vendor::Intel),
                data: &[],
            },
            CpuId {
                name: "Max Size (64-bit)",
                leaf: 0x00000012,
                subl: 0x00000000,
                func: |res| {
                    let bits = res.edx >> 8 & 0xff;
                    let (n, s) = humanize((1u64 << bits) as f64);
                    (true, Some(format!("{n:.0} {s}")))
                },
                vend: Some(Vendor::Intel),
                data: &[],
            },
            CpuId {
                name: "MiscSelect",
                leaf: 0x00000012,
                subl: 0x00000000,
                func: |res| match MiscSelect::from_bits(res.ebx) {
                    Some(ms) => (true, Some(format!("{ms:?}"))),
                    None => (false, None),
                },
                vend: Some(Vendor::Intel),
                data: &[],
            },
            CpuId {
                name: "Features",
                leaf: 0x00000012,
                subl: 0x00000001,
                func: |res| match Features::from_bits((res.ebx as u64) << 32 | res.eax as u64) {
                    Some(features) => (true, Some(format!("{features:?}"))),
                    None => (false, None),
                },
                vend: Some(Vendor::Intel),
                data: &[],
            },
            CpuId {
                name: "Xfrm",
                leaf: 0x00000012,
                subl: 0x00000001,
                func: |res| match Xfrm::from_bits((res.edx as u64) << 32 | res.ecx as u64) {
                    Some(flags) => (true, Some(format!("{flags:?}"))),
                    None => (false, None),
                },
                vend: Some(Vendor::Intel),
                data: &[],
            },
        ],
    }],
}];

pub fn epc_size(max: u32) -> Datum {
    let mut pass = false;
    let mut info = None;

    if max >= 0x00000012 {
        let mut size = 0;

        for i in 2.. {
            let result = unsafe { __cpuid_count(0x00000012, i) };
            if result.eax & 0xf != 1 {
                break;
            }

            let low = result.ecx as u64 & 0xfffff000;
            let high = result.edx as u64 & 0x000fffff;
            size += high << 12 | low;
        }

        let (n, s) = humanize(size as f64);
        info = Some(format!("{n:.0} {s}"));
        pass = true;
    }

    Datum {
        name: "EPC Size".into(),
        mesg: None,
        pass,
        info,
        data: vec![],
    }
}

pub fn dev_sgx_enclave() -> Datum {
    Datum {
        name: "Driver".into(),
        pass: File::open("/dev/sgx_enclave").is_ok(),
        info: Some("/dev/sgx_enclave".into()),
        mesg: None,
        data: vec![],
    }
}

pub fn aesm_socket() -> Datum {
    Datum {
        name: "AESM Daemon Socket".into(),
        pass: cfg!(feature = "disable-sgx-attestation") || Path::new(AESM_SOCKET).exists(),
        info: Some(AESM_SOCKET.into()),
        mesg: None,
        data: vec![],
    }
}

pub fn intel_crl() -> Datum {
    const NAME: &str = "Intel CRL cache file";
    const UPDATE_MSG: &str =
        "Run `enarx platform sgx cache-crl` to generate the Intel CRL cache file";

    let crl_file =
        match sgx_cache_dir() {
            Ok(p) => p.join("crls.der"),
            Err(e) => return Datum {
                name: NAME.to_string(),
                pass: false,
                info: Some(e.to_string()),
                mesg: Some(
                    "enarx expects the directory `/var/cache/intel-sgx` to exist and be readable"
                        .into(),
                ),
                data: vec![],
            },
        };

    if !crl_file.exists() {
        return Datum {
            name: NAME.to_string(),
            pass: false,
            info: None,
            mesg: Some(UPDATE_MSG.to_string()),
            data: vec![],
        };
    }

    let crls = match std::fs::read(crl_file.clone()) {
        Ok(c) => c,
        Err(e) => {
            return Datum {
                name: NAME.to_string(),
                pass: false,
                info: Some(e.to_string()),
                mesg: Some(UPDATE_MSG.to_string()),
                data: vec![],
            }
        }
    };

    let crls = match CrlList::from_der(&crls) {
        Ok(c) => c,
        Err(e) => {
            return Datum {
                name: NAME.to_string(),
                pass: false,
                info: Some(e.to_string()),
                mesg: Some(UPDATE_MSG.to_string()),
                data: vec![],
            }
        }
    };

    for (_, crl) in crls.entries() {
        if let Some(update) = crl.tbs_cert_list.next_update {
            if update.to_system_time() <= SystemTime::now() {
                return Datum {
                    name: NAME.to_string(),
                    pass: false,
                    info: None,
                    mesg: Some(UPDATE_MSG.to_string()),
                    data: vec![],
                };
            }
        }
    }

    if let Some(next_update) = crls.next_update() {
        Datum {
            name: NAME.to_string(),
            pass: true,
            info: Some(format!(
                "{}, next update {}",
                crl_file.to_string_lossy().into_owned(),
                next_update
            )),
            mesg: None,
            data: vec![],
        }
    } else {
        Datum {
            name: NAME.to_string(),
            pass: true,
            info: crl_file.to_string_lossy().into_owned().into(),
            mesg: None,
            data: vec![],
        }
    }
}

pub fn tcb_fmspc_cached() -> Datum {
    const NAME: &str = "TCB & FMSPC cache";
    const TCB_INSTRUCTION: &str = "Run `enarx platform sgx cache-tcb`";

    if !Path::new(FMSPC_PATH).exists() {
        return Datum {
            name: NAME.to_string(),
            pass: false,
            info: Some("Missing FMSPC".into()),
            mesg: Some("Run `enarx platform sgx cache-pck`".into()),
            data: vec![],
        };
    }

    if !Path::new(TCB_PATH).exists() {
        return Datum {
            name: NAME.to_string(),
            pass: false,
            info: Some("Missing TCB report".into()),
            mesg: Some(TCB_INSTRUCTION.into()),
            data: vec![],
        };
    }

    let tcb = match std::fs::read(TCB_PATH) {
        Ok(bytes) => bytes,
        Err(e) => {
            return Datum {
                name: NAME.to_string(),
                pass: false,
                info: Some(format!("Unable to read TCB report: {e}")),
                mesg: Some(TCB_INSTRUCTION.into()),
                data: vec![],
            };
        }
    };

    let tcb = match TcbPackage::from_der(&tcb) {
        Ok(t) => t,
        Err(e) => {
            return Datum {
                name: NAME.to_string(),
                pass: false,
                info: Some(format!("Unable to decode TCB report: {e}")),
                mesg: Some(TCB_INSTRUCTION.into()),
                data: vec![],
            };
        }
    };

    let tcb = match String::from_utf8(Vec::from(tcb.report)) {
        Ok(s) => s,
        Err(e) => {
            return Datum {
                name: NAME.to_string(),
                pass: false,
                info: Some(format!("Unable to decode JSON TCB report: {e}")),
                mesg: Some(TCB_INSTRUCTION.into()),
                data: vec![],
            };
        }
    };

    let tcb: Value = match serde_json::from_str(&tcb) {
        Ok(j) => j,
        Err(e) => {
            return Datum {
                name: NAME.to_string(),
                pass: false,
                info: Some(format!("Unable to decode JSON TCB report: {e}")),
                mesg: Some(TCB_INSTRUCTION.into()),
                data: vec![],
            };
        }
    };

    let tcb = match tcb.get("tcbInfo") {
        Some(t) => t,
        None => {
            return Datum {
                name: NAME.to_string(),
                pass: false,
                info: Some("Unable to decode JSON TCB report, missing `tcbInfo` field".into()),
                mesg: Some(TCB_INSTRUCTION.into()),
                data: vec![],
            };
        }
    };

    let next_update = match tcb.get("nextUpdate") {
        Some(t) => t,
        None => {
            return Datum {
                name: NAME.to_string(),
                pass: false,
                info: Some(
                    "Unable to decode JSON TCB report, missing `tcbInfo.nextUpdate` field".into(),
                ),
                mesg: Some(TCB_INSTRUCTION.into()),
                data: vec![],
            };
        }
    };

    let next_update = match next_update.as_str() {
        Some(u) => u,
        None => {
            return Datum {
                name: NAME.to_string(),
                pass: false,
                info: Some(
                    "Unable to decode JSON TCB report, unable to decode `tcbInfo.nextUpdate` field"
                        .into(),
                ),
                mesg: Some(TCB_INSTRUCTION.into()),
                data: vec![],
            };
        }
    };

    let next_update = match DateTime::parse_from_rfc3339(next_update) {
        Ok(d) => d,
        Err(e) => {
            return Datum {
                name: NAME.to_string(),
                pass: false,
                info: Some(format!(
                    "Unable to decode timestamp in JSON TCB report: {e}"
                )),
                mesg: Some(TCB_INSTRUCTION.into()),
                data: vec![],
            };
        }
    };

    let now = Local::now();
    if next_update < now {
        let elapsed = now.naive_local() - next_update.naive_local();
        let elapsed = {
            if elapsed.num_days() > 0 {
                format!("{} days", elapsed.num_days())
            } else {
                format!(
                    "{}:{}:{}",
                    elapsed.num_hours(),
                    elapsed.num_minutes(),
                    elapsed.num_seconds()
                )
            }
        };
        return Datum {
            name: NAME.to_string(),
            pass: false,
            info: Some(format!("Intel TCB expired on {next_update}, {elapsed} ago")),
            mesg: Some(TCB_INSTRUCTION.into()),
            data: vec![],
        };
    }

    Datum {
        name: NAME.to_string(),
        pass: true,
        info: Some(format!("Next update: {next_update}")),
        mesg: None,
        data: vec![],
    }
}
