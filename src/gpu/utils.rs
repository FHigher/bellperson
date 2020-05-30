use crate::gpu::error::{GPUError, GPUResult};
use ocl::{Device, Platform};

use log::info;
use std::collections::HashMap;
use std::env;

pub const GPU_NVIDIA_PLATFORM_NAME: &str = "NVIDIA CUDA";
// pub const CPU_INTEL_PLATFORM_NAME: &str = "Intel(R) CPU Runtime for OpenCL(TM) Applications";

pub fn get_devices(platform_name: &str) -> GPUResult<Vec<Device>> {
    if env::var("BELLMAN_NO_GPU").is_ok() {
        return Err(GPUError::Simple("GPU accelerator is disabled!"));
    }

    let platform = Platform::list()?.into_iter().find(|&p| match p.name() {
        Ok(p) => p == platform_name,
        Err(_) => false,
    });

    let bus_ids = env::var("BELLMAN_GPUS").map(|v| {
        v.split(",")
            .map(|s| s.parse::<u32>().expect("Invalid Bus-Id number!"))
            .collect::<Vec<u32>>()
    });

    match platform {
        Some(p) => {
            let mut devs = Device::list_all(p)?;
            if let Ok(bus_ids) = bus_ids {
                let mut filtered_devs = Vec::new();
                for d in devs.iter() {
                    if bus_ids.contains(&get_bus_id(*d)?) {
                        filtered_devs.push(*d);
                    }
                }
                devs = filtered_devs;
            }
            Ok(devs)
        }
        None => Err(GPUError::Simple("GPU platform not found!")),
    }
}

lazy_static::lazy_static! {
    static ref CORE_COUNTS: HashMap<String, usize> = {
        let mut core_counts : HashMap<String, usize> = vec![
            ("TITAN RTX".to_string(), 4608),

            ("Tesla V100".to_string(), 5120),
            ("Tesla P100".to_string(), 3584),
            ("Tesla T4".to_string(), 2560),

            ("GeForce RTX 2080 Ti".to_string(), 4352),
            ("GeForce RTX 2080 SUPER".to_string(), 3072),
            ("GeForce RTX 2080".to_string(), 2944),
            ("GeForce RTX 2070 SUPER".to_string(), 2560),

            ("GeForce GTX 1080 Ti".to_string(), 3584),
            ("GeForce GTX 1080".to_string(), 2560),
            ("GeForce GTX 2060".to_string(), 1920),
            ("GeForce GTX 1660 Ti".to_string(), 1536),
            ("GeForce GTX 1060".to_string(), 1280),
            ("GeForce GTX 1650 SUPER".to_string(), 1280),
            ("GeForce GTX 1650".to_string(), 896),
        ].into_iter().collect();

        match env::var("BELLMAN_CUSTOM_GPU").and_then(|var| {
            for card in var.split(",") {
                let splitted = card.split(":").collect::<Vec<_>>();
                if splitted.len() != 2 { panic!("Invalid BELLMAN_CUSTOM_GPU!"); }
                let name = splitted[0].trim().to_string();
                let cores : usize = splitted[1].trim().parse().expect("Invalid BELLMAN_CUSTOM_GPU!");
                info!("Adding \"{}\" to GPU list with {} CUDA cores.", name, cores);
                core_counts.insert(name, cores);
            }
            Ok(())
        }) { Err(_) => { }, Ok(_) => { } }

        core_counts
    };
}

pub fn get_core_count(d: Device) -> GPUResult<usize> {
    match CORE_COUNTS.get(&d.name()?[..]) {
        Some(&cores) => Ok(cores),
        None => Err(GPUError::Simple("Device unknown!")),
    }
}

pub fn get_bus_id(d: Device) -> GPUResult<u32> {
    let result = d.info_raw(0x4008)?;
    Ok((result[0] as u32)
        + ((result[1] as u32) << 8)
        + ((result[2] as u32) << 16)
        + ((result[3] as u32) << 24))
}

pub fn get_memory(d: Device) -> GPUResult<u64> {
    match d.info(ocl::enums::DeviceInfo::GlobalMemSize)? {
        ocl::enums::DeviceInfoResult::GlobalMemSize(sz) => Ok(sz),
        _ => Err(GPUError::Simple("Cannot extract GPU memory!")),
    }
}
