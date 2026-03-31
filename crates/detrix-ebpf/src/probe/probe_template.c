// BPF uprobe template — auto-completed by detrix-ebpf/probe/program.rs at runtime.
//
// Placeholders replaced before compilation:
//   DETRIX_EVENT_FIELDS  → per-metric variable fields (u64 var0, …)
//   DETRIX_VAR_READS     → per-metric bpf_probe_read_user / register reads
//
// Verify this template compiles on its own (placeholders produce an empty-but-valid program):
//   clang -O2 -target bpf -D__TARGET_ARCH_arm64 -c probe_template.c -o /dev/null
//   clang -O2 -target bpf -D__TARGET_ARCH_x86   -c probe_template.c -o /dev/null

#include <linux/bpf.h>
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>

// linux/types.h (via linux/bpf.h) defines __u32/__u64 but NOT u32/u64 in userspace.
typedef __u8 u8;
typedef __u32 u32;
typedef __u64 u64;

// bpf_helper_defs.h only forward-declares struct pt_regs; complete it here.
// asm/ptrace.h is unreliable in slim containers (arm64 exposes user_pt_regs, not pt_regs).
// Caller passes -D__TARGET_ARCH_arm64 or -D__TARGET_ARCH_x86 (see loader.rs).
#if defined(__TARGET_ARCH_arm64)
// arm64: layout matches struct user_pt_regs — what the kernel passes to BPF uprobes.
struct pt_regs {
    unsigned long long regs[31];
    unsigned long long sp;
    unsigned long long pc;
    unsigned long long pstate;
};
#elif defined(__TARGET_ARCH_x86) || defined(__TARGET_ARCH_x86_64)
// x86-64: layout matches kernel struct pt_regs for BPF uprobes.
struct pt_regs {
    unsigned long long r15, r14, r13, r12, rbp, rbx;
    unsigned long long r11, r10, r9, r8;
    unsigned long long rax, rcx, rdx, rsi, rdi;
    unsigned long long orig_rax;
    unsigned long long rip, cs, eflags, rsp, ss;
};
#endif

char LICENSE[] SEC("license") = "Dual MIT/GPL";

struct probe_event {
    u32 pid;
    u32 tid;
    u64 timestamp;
    /*DETRIX_EVENT_FIELDS*/
};

struct {
    __uint(type, BPF_MAP_TYPE_RINGBUF);
    __uint(max_entries, 256 * 1024); // 256 KB
} DETRIX_EVENTS SEC(".maps");

SEC("uprobe")
int detrix_capture(struct pt_regs *ctx) {
    struct probe_event *event;
    event = bpf_ringbuf_reserve(&DETRIX_EVENTS, sizeof(*event), 0);
    if (!event) return 0;

    u64 pid_tgid = bpf_get_current_pid_tgid();
    event->pid = pid_tgid >> 32;
    event->tid = (u32)pid_tgid;
    event->timestamp = bpf_ktime_get_ns();

    /*DETRIX_VAR_READS*/

    bpf_ringbuf_submit(event, 0);
    return 0;
}
