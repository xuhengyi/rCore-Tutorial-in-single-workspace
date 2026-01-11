use crate::{fs_pack::easy_fs_pack, get_target_dir, objcopy, Arch, PROJECT};
use os_xtask_utils::{Cargo, CommandExt};
use serde_derive::Deserialize;
use std::{collections::HashMap, ffi::OsStr, fs::File, io::Write, path::PathBuf};

#[derive(Deserialize, Default)]
struct Cases {
    base: Option<u64>,
    step: Option<u64>,
    pub cases: Option<Vec<String>>,
}

pub struct CasesInfo {
    base: u64,
    step: u64,
    bins: Vec<PathBuf>,
}

impl Cases {
    fn build(&mut self, release: bool, target_arch: &str) -> CasesInfo {
        if let Some(names) = &self.cases {
            let base = self.base.unwrap_or(0);
            let step = self.step.filter(|_| self.base.is_some()).unwrap_or(0);
            let cases = names
                .into_iter()
                .enumerate()
                .map(|(i, name)| build_one(name, release, base + i as u64 * step, target_arch))
                .collect();
            CasesInfo {
                base,
                step,
                bins: cases,
            }
        } else {
            CasesInfo {
                base: 0,
                step: 0,
                bins: vec![],
            }
        }
    }
}

fn build_one(name: impl AsRef<OsStr>, release: bool, base_address: u64, target_arch: &str) -> PathBuf {
    let name = name.as_ref();
    let binary = base_address != 0;
    if binary {
        println!("build {name:?} at {base_address:#x}");
    }
    Cargo::build()
        .package("user_lib")
        .target(target_arch)
        .arg("--bin")
        .arg(name)
        .conditional(release, |cargo| {
            cargo.release();
        })
        .conditional(binary, |cargo| {
            cargo.env("BASE_ADDRESS", base_address.to_string());
        })
        .invoke();
    let elf = get_target_dir(target_arch)
        .join(if release { "release" } else { "debug" })
        .join(name);
    if binary {
        objcopy(elf, binary)
    } else {
        elf
    }
}

pub fn build_for(ch: u8, release: bool, kernel_arch: Arch) {
    // 用户程序的目标架构与内核保持一致
    let target_arch = kernel_arch.target();
    let target_dir = get_target_dir(target_arch);
    
    let cfg = std::fs::read_to_string(PROJECT.join("user/cases.toml")).unwrap();
    let mut cases = toml::from_str::<HashMap<String, Cases>>(&cfg)
        .unwrap()
        .remove(&format!("ch{ch}"))
        .unwrap_or_default();
    let CasesInfo { base, step, bins } = cases.build(release, target_arch);
    if bins.is_empty() {
        return;
    }
    
    let asm = target_dir
        .join(if release { "release" } else { "debug" })
        .join("app.asm");
    let mut ld = File::create(asm).unwrap();
    
    // 根据内核架构选择数据宽度指令
    // RV64 使用 .quad (8字节), RV32 使用 .word (4字节)
    let (data_directive, align_size) = match kernel_arch {
        Arch::Riscv64 => (".quad", 3),  // 8字节对齐
        Arch::Riscv32 => (".word", 2),  // 4字节对齐
    };
    
    writeln!(
        ld,
        "\
    .global apps
    .section .data
    .align {align_size}
apps:
    {data_directive} {base:#x}
    {data_directive} {step:#x}
    {data_directive} {}",
        bins.len(),
    )
    .unwrap();

    (0..bins.len()).for_each(|i| writeln!(ld, "    {data_directive} app_{i}_start").unwrap());

    writeln!(ld, "    {data_directive} app_{}_end", bins.len() - 1).unwrap();

    bins.iter().enumerate().for_each(|(i, path)| {
        writeln!(
            ld,
            "
app_{i}_start:
    .incbin {path:?}
app_{i}_end:",
        )
        .unwrap();
    });

    if ch == 5 {
        writeln!(
            ld,
            "
    .align {align_size}
    .section .data
    .global app_names
app_names:"
        )
        .unwrap();
        bins.iter().enumerate().for_each(|(_, path)| {
            writeln!(ld, "    .string {:?}", path.file_name().unwrap()).unwrap();
        });
    } else if ch >= 6 {
        easy_fs_pack(
            &cases.cases.unwrap(),
            target_dir
                .join(if release { "release" } else { "debug" })
                .into_os_string()
                .into_string()
                .unwrap()
                .as_str(),
        )
        .unwrap();
    }
}
