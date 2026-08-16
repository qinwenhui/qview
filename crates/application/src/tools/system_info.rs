//! `system_info` 工具：只读查看当前系统信息（OS / 内存 / CPU / 磁盘 / 网络）。
//!
//! 数据全部来自 `sysinfo` crate（跨平台 Windows / macOS / Linux），
//! 不自己写 sysctl / /proc / GetDiskFreeSpaceEx 那套平台分支。
//! 输出统一用原始单位：字节（`*_bytes`）/ 秒（`uptime_secs`）/ MHz（`frequency_mhz`），
//! 由 LLM 负责换算成 GB 汇报，避免精度丢失。

use futures::future::FutureExt;
use serde_json::{json, Value};

use contexa_tools::{boxed_invoke, LocalTool, ToolResult};
use sysinfo::{
    CpuRefreshKind, Disks, MemoryRefreshKind, Networks, RefreshKind, System,
    MINIMUM_CPU_UPDATE_INTERVAL,
};

use crate::protocol::SideEffect;
use crate::tool::metadata::{ToolGroup, ToolMetadata};

/// 工具元数据。
pub fn system_info_metadata() -> ToolMetadata {
    ToolMetadata::new(
        "system_info",
        "查看当前系统信息：OS 类型/版本、内核、主机名、CPU、内存、磁盘、网络（scope 可选：all/os/memory/cpu/disk/network）",
        SideEffect::ReadOnly,
        ToolGroup::System,
    )
}

/// 工具入参 JSON Schema。
pub fn system_info_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "scope": {
                "type": "string",
                "enum": ["all", "os", "memory", "cpu", "disk", "network"],
                "default": "all",
                "description": "要返回的信息范围：all=全部；os=OS 与主机；memory=内存与 swap；cpu=CPU 品牌/核数/频率/占用；disk=各挂载点空间；network=网卡列表"
            }
        },
        "required": [],
        "additionalProperties": false
    })
}

/// 构造工具。只读，无需捕获任何 Service。
pub fn system_info_tool() -> anyhow::Result<LocalTool> {
    let tool = LocalTool::from_async_fn(
        "system_info",
        "查看当前系统信息：OS 类型/版本、内核、主机名、CPU、内存、磁盘、网络（scope 可选：all/os/memory/cpu/disk/network）",
        system_info_parameters(),
        boxed_invoke(move |args| {
            async move {
                let scope = args
                    .get("scope")
                    .and_then(|v| v.as_str())
                    .unwrap_or("all")
                    .trim()
                    .to_ascii_lowercase();

                let payload = match scope.as_str() {
                    "all" => all_payload().await,
                    "os" => json!({ "os": os_payload() }),
                    "memory" => json!({ "memory": memory_payload() }),
                    "cpu" => json!({ "cpu": cpu_payload().await }),
                    "disk" => json!({ "disk": disk_payload() }),
                    "network" => json!({ "network": network_payload() }),
                    other => {
                        return Ok(ToolResult::err(json!({
                            "error": "invalid_scope",
                            "scope": other,
                            "valid_scopes": ["all", "os", "memory", "cpu", "disk", "network"]
                        })));
                    }
                };
                Ok(ToolResult::ok(payload))
            }
            .boxed()
        }),
    )?;
    Ok(tool)
}

/// 全量：OS + 内存 + CPU + 磁盘 + 网络。
async fn all_payload() -> Value {
    // CPU 占用基于两次采样差值：睡一个采样间隔再 refresh，usage 才有意义。
    let cpu = cpu_payload().await;
    json!({
        "os": os_payload(),
        "memory": memory_payload(),
        "cpu": cpu,
        "disk": disk_payload(),
        "network": network_payload(),
    })
}

/// OS / 主机信息（sysinfo 静态方法，无需 refresh 实例）。
fn os_payload() -> Value {
    json!({
        "os_type": System::name(),
        "os_version": System::os_version(),
        "long_os_version": System::long_os_version(),
        "distribution_id": System::distribution_id(),
        "kernel_version": System::kernel_version(),
        "cpu_arch": System::cpu_arch(),
        "hostname": System::host_name(),
        "uptime_secs": System::uptime(),
    })
}

/// 内存 / swap（字节）。
fn memory_payload() -> Value {
    let mut sys = System::new_with_specifics(
        RefreshKind::nothing().with_memory(MemoryRefreshKind::everything()),
    );
    sys.refresh_memory_specifics(MemoryRefreshKind::everything());
    json!({
        "total_bytes": sys.total_memory(),
        "used_bytes": sys.used_memory(),
        "free_bytes": sys.free_memory(),
        "available_bytes": sys.available_memory(),
        "swap_total_bytes": sys.total_swap(),
        "swap_used_bytes": sys.used_swap(),
        "swap_free_bytes": sys.free_swap(),
    })
}

/// CPU：品牌 / 物理·逻辑核数 / 各核频率与使用率。
async fn cpu_payload() -> Value {
    let mut sys = System::new_with_specifics(
        RefreshKind::nothing().with_cpu(CpuRefreshKind::everything()),
    );
    // CPU usage 是两次采样差值：等一个间隔再 refresh 才能拿到实际占用。
    tokio::time::sleep(MINIMUM_CPU_UPDATE_INTERVAL).await;
    sys.refresh_cpu_usage();

    let cpus: Vec<Value> = sys
        .cpus()
        .iter()
        .enumerate()
        .map(|(idx, c)| {
            json!({
                "index": idx,
                "name": c.name(),
                "brand": c.brand(),
                "frequency_mhz": c.frequency(),
                "usage_percent": c.cpu_usage(),
            })
        })
        .collect();

    let logical = cpus.len();
    let usage_sum: f32 = sys.cpus().iter().map(|c| c.cpu_usage()).sum();
    let avg_usage = if logical == 0 { 0.0 } else { usage_sum / logical as f32 };

    let brand = sys
        .cpus()
        .first()
        .map(|c| c.brand().to_string())
        .unwrap_or_default();

    json!({
        "brand": if brand.is_empty() { Value::Null } else { Value::String(brand) },
        "physical_cores": System::physical_core_count(),
        "logical_cores": logical,
        "usage_percent": avg_usage,
        "cpus": cpus,
    })
}

/// 磁盘：每个挂载点的文件系统 / 总·可用·已用空间 / 是否可移除 / 挂载点。
fn disk_payload() -> Value {
    let disks = Disks::new_with_refreshed_list();
    let mounts: Vec<Value> = disks
        .list()
        .iter()
        .map(|d| {
            let total = d.total_space();
            let available = d.available_space();
            json!({
                "file_system": d.file_system().to_string_lossy(),
                "total_bytes": total,
                "available_bytes": available,
                "used_bytes": total.saturating_sub(available),
                "is_removable": d.is_removable(),
                "mount_point": d.mount_point().to_string_lossy(),
                "name": d.name().to_string_lossy(),
            })
        })
        .collect();
    json!({ "mounts": mounts })
}

/// 网络：各网卡的名称 / 是否 up / MAC / IP 列表。
fn network_payload() -> Value {
    let networks = Networks::new_with_refreshed_list();
    let mut interfaces: Vec<Value> = networks
        .list()
        .iter()
        .map(|(name, data)| {
            // sysinfo 0.36 没有 `operational_state` API（0.39 才加），
            // 用 mtu + 累计收发包数做"网卡是否在用"的启发式判断。
            // 0.36 默认 mtu=0 表示未读取到 MTU，累计包=0 表示从开机起没收发过。
            let is_up = data.mtu() > 0
                && (data.total_packets_received() > 0 || data.total_packets_transmitted() > 0);
            json!({
                "name": name,
                "is_up": is_up,
                "operational_state": "unknown",
                "mac_address": data.mac_address().to_string(),
                "ip_addresses": data
                    .ip_networks()
                    .iter()
                    .map(|ip| ip.to_string())
                    .collect::<Vec<String>>(),
            })
        })
        .collect();
    interfaces.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    json!({ "interfaces": interfaces })
}
