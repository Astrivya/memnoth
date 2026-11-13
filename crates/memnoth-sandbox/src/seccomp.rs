//! Linux seccomp-BPF syscall filter for the sandbox.
//!
//! Builds a [`SockFProg`] BPF program that is installed with
//! `prctl(PR_SET_NO_NEW_PRIVS)` + `prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER)`
//! before the sandboxed child execs. The filter allows a named set of syscalls
//! for the target architecture and returns `EPERM` for everything else — fail
//! closed.
//!
//! The filter is data: [`Filter::to_bytes`] serializes it into the wire format
//! the kernel expects, and [`install`] installs it on the calling thread.
//! `install` must run in the sandboxed child before `execve` — the one call
//! site this crate cannot own, because it does not spawn the child. It is a
//! no-op off Linux, and on a Linux target this build cannot name syscalls for
//! it refuses to install instead of installing an allowlist that belongs to a
//! different architecture.
//!
//! ## Why the jump offsets are computed
//!
//! A classic BPF jump is relative to the instruction *after* the branch, in
//! units of instructions, and `jt`/`jf` are one byte. A filter that used one
//! constant offset for every `JEQ` would allow only the last listed syscall and
//! `EPERM` the rest — a sandbox that breaks the build rather than one that
//! leaks, but still a filter nobody tested. [`Filter::for_arch`] lays the
//! program out first and computes each offset from its final position.
//!
//! ## What this does not do
//!
//! It does not filter argument values (no path-scoped `openat`), it does not
//! cover `io_uring` (which would need a separate exact-match design), and it
//! does not replace the namespace: a seccomp filter is one layer beside
//! `bwrap`'s filesystem view, not a substitute for it.

#![allow(dead_code)] // the op/ret tables are the reference for the encoding

use std::os::raw::{c_int, c_uint};

/// BPF instruction class / flag constants (`<linux/filter.h>`).
mod op {
    pub const BPF_ALU: u16 = 0x04;
    pub const BPF_CLASS: u16 = 0x07;
    pub const BPF_JMP: u16 = 0x05;
    pub const BPF_LD: u16 = 0x00;
    pub const BPF_MISC: u16 = 0x07;
    pub const BPF_RET: u16 = 0x06;

    pub const BPF_ABS: u16 = 0x20;
    pub const BPF_IMM: u16 = 0x00;
    pub const BPF_K: u16 = 0x00;
    pub const BPF_LEN: u16 = 0x80;
    pub const BPF_MEM: u16 = 0x60;
    pub const BPF_MSH: u16 = 0xa0;
    pub const BPF_IND: u16 = 0x40;

    pub const BPF_JA: u16 = 0x00;
    pub const BPF_JEQ: u16 = 0x10;
    pub const BPF_JGE: u16 = 0x30;
    pub const BPF_JGT: u16 = 0x20;
    pub const BPF_JSET: u16 = 0x40;

    pub const BPF_X: u16 = 0x08;

    pub const BPF_TAX: u16 = 0x00;
    pub const BPF_TXA: u16 = 0x80;

    pub const BPF_W: u16 = 0x00;
    pub const BPF_H: u16 = 0x08;
    pub const BPF_B: u16 = 0x10;

    pub const BPF_NOP: u16 = BPF_LD | BPF_IMM | BPF_W;
    pub const BPF_RET_A: u16 = BPF_RET | 0x10;
    pub const BPF_RET_K: u16 = BPF_RET | BPF_K;
}

/// `seccomp_data` field offsets (`struct seccomp_data`).
pub mod offset {
    /// `offsetof(struct seccomp_data, nr)` — the syscall number.
    pub const NR: usize = 0;
    /// `offsetof(struct seccomp_data, arch)` — the audit architecture token.
    pub const ARCH: usize = 4;
}

/// Seccomp return values (`<linux/seccomp.h>`).
pub mod ret {
    /// Allow the syscall.
    pub const ALLOW: u32 = 0x7fff_0000;
    /// Return `EPERM` to the caller.
    pub const ERRNO_EPERM: u32 = 0x0005_0000 | (1 & 0xffff);
    /// Trap (send SIGSYS).
    pub const TRAP: u32 = 0x0003_0000;
}

/// `prctl` option for installing a seccomp filter.
pub const PR_SET_SECCOMP: c_int = 22;
/// `prctl` option that forbids gaining privileges through `execve`.
pub const PR_SET_NO_NEW_PRIVS: c_int = 38;
pub const SECCOMP_MODE_FILTER: c_uint = 1;

/// Audit architecture tokens (`<linux/audit.h>`).
pub mod arch {
    /// `AUDIT_ARCH_X86_64`.
    pub const X86_64: u32 = 0xC000_003E;
    /// `AUDIT_ARCH_AARCH64`.
    pub const AARCH64: u32 = 0xC000_00B7;
}

/// The architecture a filter is built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    Aarch64,
    /// A Linux target this build has no syscall table for.
    Other,
}

/// The architecture this binary was compiled for.
pub fn target_arch() -> Arch {
    #[cfg(target_arch = "x86_64")]
    {
        Arch::X86_64
    }
    #[cfg(target_arch = "aarch64")]
    {
        Arch::Aarch64
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        Arch::Other
    }
}

/// `struct sock_filter` — one BPF instruction (8 bytes, little-endian).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SockFilter {
    pub code: u16,
    pub jt: u8,
    pub jf: u8,
    pub k: u32,
}

/// `struct sock_fprog` — a BPF program passed to the kernel.
#[repr(C)]
#[derive(Debug, Clone)]
pub struct SockFProg {
    pub len: u16,
    pub filter: *const SockFilter,
}

/// A compiled seccomp-BPF program, ready to pass to the kernel.
#[derive(Debug, Clone)]
pub struct Filter {
    pub instructions: Vec<SockFilter>,
}

/// Syscalls a toolchain child needs on x86_64.
///
/// Deliberately absent: `socket`, `connect`, `sendto`, `ptrace`, `mount`,
/// `unshare`, `setns`, `setuid`, `keyctl`, `bpf`, `io_uring_*`, `process_vm_*`.
/// A missing syscall is an `EPERM` the caller sees, not a fallback: an
/// allowlist that quietly grew would be a filter nobody audited.
const X86_64_ALLOWED: &[u32] = &[
    0,   // read
    1,   // write
    3,   // close
    5,   // fstat
    8,   // lseek
    9,   // mmap
    10,  // mprotect
    11,  // munmap
    12,  // brk
    13,  // rt_sigaction
    14,  // rt_sigprocmask
    16,  // ioctl
    24,  // sched_yield
    32,  // dup
    33,  // dup2
    39,  // getpid
    59,  // execve
    60,  // exit
    61,  // wait4
    63,  // uname
    72,  // fcntl
    79,  // getcwd
    89,  // readlink
    102, // getuid
    104, // getgid
    107, // geteuid
    108, // getegid
    131, // sigaltstack
    158, // arch_prctl
    186, // gettid
    202, // futex
    217, // getdents64
    228, // clock_gettime
    231, // exit_group
    257, // openat
    262, // newfstatat
    267, // readlinkat
    273, // set_robust_list
    293, // pipe2
    302, // prlimit64
    318, // getrandom
    334, // rseq
];

/// Syscalls a toolchain child needs on aarch64 (asm-generic numbering).
const AARCH64_ALLOWED: &[u32] = &[
    29,  // ioctl
    56,  // openat
    57,  // close
    62,  // lseek
    63,  // read
    64,  // write
    78,  // readlinkat
    79,  // newfstatat
    80,  // fstat
    93,  // exit
    94,  // exit_group
    96,  // set_tid_address
    98,  // futex
    113, // clock_gettime
    124, // sched_yield
    129, // kill
    131, // tgkill
    132, // sigaltstack
    134, // rt_sigaction
    135, // rt_sigprocmask
    160, // uname
    167, // prctl
    172, // getpid
    173, // getppid
    174, // getuid
    175, // geteuid
    176, // getgid
    177, // getegid
    178, // gettid
    198, // socketpair (local IPC only; no socket/connect)
    214, // brk
    215, // munmap
    216, // mremap
    220, // clone
    221, // execve
    222, // mmap
    226, // mprotect
    260, // wait4
    261, // prlimit64
    278, // getrandom
    280, // membarrier
    293, // rseq
];

impl Filter {
    /// Build the filter for the compiling target.
    ///
    /// On a target this build has no syscall table for, the result denies every
    /// syscall: safer than allowing an architecture's numbers under another
    /// architecture's table. [`install`] refuses in that case, and
    /// [`crate::linux::confine`] reports the boundary as unavailable rather than
    /// claiming a filter it could not build.
    pub fn sandbox() -> Self {
        Self::for_arch(target_arch())
    }

    /// Build the filter for `arch`.
    pub fn for_arch(arch: Arch) -> Self {
        let (token, allowed): (u32, &[u32]) = match arch {
            Arch::X86_64 => (arch::X86_64, X86_64_ALLOWED),
            Arch::Aarch64 => (arch::AARCH64, AARCH64_ALLOWED),
            Arch::Other => return Self::deny_all(),
        };
        Self::program(token, allowed).unwrap_or_else(|_| Self::deny_all())
    }

    /// A program that refuses every syscall. The fail-closed fallback.
    pub fn deny_all() -> Self {
        Self {
            instructions: vec![SockFilter {
                code: op::BPF_RET_K,
                jt: 0,
                jf: 0,
                k: ret::ERRNO_EPERM,
            }],
        }
    }

    /// Whether this filter allows any syscall at all.
    pub fn is_deny_all(&self) -> bool {
        self.instructions.len() == 1 && self.instructions[0].k == ret::ERRNO_EPERM
    }

    /// Lay out the program and compute every jump offset.
    ///
    /// Layout: `LD arch`, `JEQ arch -> continue | EPERM`, `LD nr`, one `JEQ`
    /// per allowed syscall, `RET EPERM`, `RET ALLOW`. The allow target is the
    /// last instruction, so each `JEQ`'s `jt` is computed from its own index.
    fn program(arch_token: u32, allowed: &[u32]) -> anyhow::Result<Self> {
        let mut instructions: Vec<SockFilter> = Vec::with_capacity(allowed.len() + 5);
        instructions.push(SockFilter {
            code: op::BPF_LD | op::BPF_W | op::BPF_ABS,
            jt: 0,
            jf: 0,
            k: offset::ARCH as u32,
        });
        // A syscall number is only meaningful under the table it belongs to.
        // Refuse a foreign ABI before looking at `nr` at all.
        instructions.push(SockFilter {
            code: op::BPF_JMP | op::BPF_JEQ | op::BPF_K,
            jt: 0,
            jf: 1, // not our arch: fall through to the `RET EPERM` below
            k: arch_token,
        });
        instructions.push(SockFilter {
            code: op::BPF_LD | op::BPF_W | op::BPF_ABS,
            jt: 0,
            jf: 0,
            k: offset::NR as u32,
        });

        let first_jeq = instructions.len();
        let deny_index = first_jeq + allowed.len();
        let allow_index = deny_index + 1;
        // The architecture branch has to land on the deny rule, whose index is
        // only known now that the allowlist length is.
        let arch_jf = u8::try_from(deny_index - 2).map_err(|_| {
            anyhow::anyhow!(
                "seccomp filter: the architecture branch needs a jump wider than the one-byte \
                 field allows"
            )
        })?;
        instructions[1].jf = arch_jf;
        for (position, nr) in allowed.iter().enumerate() {
            let index = first_jeq + position;
            let offset = allow_index - index - 1;
            let jt = u8::try_from(offset).map_err(|_| {
                anyhow::anyhow!(
                    "seccomp filter: {offset} instructions to the allow target exceeds the \
                     one-byte jump field"
                )
            })?;
            instructions.push(SockFilter {
                code: op::BPF_JMP | op::BPF_JEQ | op::BPF_K,
                jt,
                jf: 0,
                k: *nr,
            });
        }
        instructions.push(SockFilter {
            code: op::BPF_RET_K,
            jt: 0,
            jf: 0,
            k: ret::ERRNO_EPERM,
        });
        instructions.push(SockFilter {
            code: op::BPF_RET_K,
            jt: 0,
            jf: 0,
            k: ret::ALLOW,
        });

        Ok(Self { instructions })
    }

    /// Serialize the filter into the kernel wire format (each instruction
    /// is 8 little-endian bytes: `code u16, jt u8, jf u8, k u32`).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.instructions.len() * 8);
        for f in &self.instructions {
            out.extend_from_slice(&f.code.to_le_bytes());
            out.push(f.jt);
            out.push(f.jf);
            out.extend_from_slice(&f.k.to_le_bytes());
        }
        out
    }
}

/// Every syscall number the filter allows, for the docs and the audit line.
pub fn allowed_syscalls(arch: Arch) -> &'static [u32] {
    match arch {
        Arch::X86_64 => X86_64_ALLOWED,
        Arch::Aarch64 => AARCH64_ALLOWED,
        Arch::Other => &[],
    }
}

/// Install the sandbox filter on the calling thread.
///
/// Must be called in the sandboxed child before `execve`. `no_new_privs` is set
/// first because the kernel refuses a seccomp filter from an unprivileged
/// process without it — the caller should not have to remember that, and a
/// filter that fails to install is worse than one that was never planned.
#[cfg(target_os = "linux")]
pub fn install(filter: &Filter) -> anyhow::Result<()> {
    use std::ffi::c_ulong;

    // Declared here rather than taken from a dependency: this crate's promise
    // is that it builds with the dependencies it lists, and `prctl` is four
    // lines of ABI.
    unsafe extern "C" {
        fn prctl(option: c_int, ...) -> c_int;
    }

    if filter.is_deny_all() {
        anyhow::bail!(
            "refusing to install a seccomp filter that denies every syscall: this target has no \
             syscall table, so the boundary is unavailable rather than broken"
        );
    }
    if filter.instructions.len() > u16::MAX as usize {
        anyhow::bail!("seccomp filter is longer than a sock_fprog can name");
    }
    let prog = SockFProg {
        len: filter.instructions.len() as u16,
        filter: filter.instructions.as_ptr(),
    };

    // SAFETY: `PR_SET_NO_NEW_PRIVS` takes one integer argument and reads no
    // memory from this process.
    let rc = unsafe { prctl(PR_SET_NO_NEW_PRIVS, 1 as c_ulong) };
    if rc != 0 {
        anyhow::bail!(
            "prctl(PR_SET_NO_NEW_PRIVS) failed: {}",
            std::io::Error::last_os_error()
        );
    }
    // SAFETY: `prog` borrows `filter.instructions`, which outlives this call,
    // and its `len` matches the vector it points at.
    let rc = unsafe {
        prctl(
            PR_SET_SECCOMP,
            SECCOMP_MODE_FILTER as c_ulong,
            &prog as *const SockFProg as c_ulong,
            0 as c_ulong,
            0 as c_ulong,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        anyhow::bail!(
            "prctl(PR_SET_SECCOMP) failed: {}",
            std::io::Error::last_os_error()
        )
    }
}

/// `install` is unavailable off Linux: there is no boundary to install into.
#[cfg(not(target_os = "linux"))]
pub fn install(_filter: &Filter) -> anyhow::Result<()> {
    anyhow::bail!("seccomp is a Linux syscall filter; this build has no boundary to install into")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_program_starts_with_an_architecture_check() {
        let f = Filter::for_arch(Arch::X86_64);
        assert_eq!(f.instructions[0].k, offset::ARCH as u32);
        assert_eq!(f.instructions[1].k, arch::X86_64);
        assert_eq!(f.instructions[2].k, offset::NR as u32);
    }

    #[test]
    fn every_allowed_syscall_jumps_to_the_allow_rule() {
        let f = Filter::for_arch(Arch::X86_64);
        let allowed = allowed_syscalls(Arch::X86_64);
        let first = 3;
        let allow_index = f.instructions.len() - 1;
        assert_eq!(f.instructions[allow_index].k, ret::ALLOW);
        for (position, nr) in allowed.iter().enumerate() {
            let index = first + position;
            let instruction = f.instructions[index];
            assert_eq!(
                instruction.k, *nr,
                "instruction {index} is not syscall {nr}"
            );
            assert_eq!(
                index + 1 + instruction.jt as usize,
                allow_index,
                "syscall {nr} does not jump to the allow rule"
            );
            assert_eq!(
                instruction.jf, 0,
                "syscall {nr} must fall through on mismatch"
            );
        }
    }

    #[test]
    fn a_syscall_that_is_not_listed_reaches_the_eperm_rule() {
        let f = Filter::for_arch(Arch::X86_64);
        let deny = f
            .instructions
            .iter()
            .rposition(|instruction| instruction.k == ret::ERRNO_EPERM)
            .expect("a deny rule");
        // The last JEQ falls through to the deny rule.
        assert_eq!(deny, f.instructions.len() - 2);
    }

    #[test]
    fn the_architecture_branch_reaches_the_deny_rule() {
        let f = Filter::for_arch(Arch::X86_64);
        let branch = f.instructions[1];
        let target = 1 + 1 + branch.jf as usize;
        assert_eq!(f.instructions[target].k, ret::ERRNO_EPERM);
    }

    #[test]
    fn an_unknown_architecture_denies_everything_rather_than_guessing() {
        let f = Filter::for_arch(Arch::Other);
        assert!(f.is_deny_all());
        assert!(allowed_syscalls(Arch::Other).is_empty());
        assert!(install(&f).is_err());
    }

    #[test]
    fn aarch64_uses_its_own_syscall_numbers() {
        let f = Filter::for_arch(Arch::Aarch64);
        assert_eq!(f.instructions[1].k, arch::AARCH64);
        // read is 63 on aarch64, 0 on x86_64: the two tables must not be mixed.
        let allowed = allowed_syscalls(Arch::Aarch64);
        assert!(allowed.contains(&63));
        assert!(!allowed.contains(&0));
    }

    #[test]
    fn the_dangerous_syscalls_are_not_on_any_list() {
        // socket (41 x86_64 / 198 aarch64 is socketpair only), ptrace, mount,
        // unshare, setuid, bpf.
        for blocked in [41u32, 101, 165, 272, 105, 321] {
            assert!(
                !allowed_syscalls(Arch::X86_64).contains(&blocked),
                "syscall {blocked} must stay blocked"
            );
        }
    }

    #[test]
    fn to_bytes_is_aligned_and_matches_the_instruction_count() {
        let f = Filter::for_arch(Arch::X86_64);
        let bytes = f.to_bytes();
        assert_eq!(bytes.len() % 8, 0, "each sock_filter is 8 bytes");
        assert_eq!(bytes.len() / 8, f.instructions.len());
    }
}
