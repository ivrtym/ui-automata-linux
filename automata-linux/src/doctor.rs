use std::collections::BTreeSet;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

use crate::a11y::A11yClient;
use crate::backend::{
    DisplayBackend, PlatformDiagnostics, command_exists, find_ocr_model_dir, find_ort_dylib,
    platform_diagnostics, resolve_display_backend, uinput_writable, ydotoold_running,
};
use crate::{Result, UiError};

/// Target automation profile to check or prepare.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvProfile {
    /// Inspect components needed for the active display session plus OCR & AT-SPI2.
    Auto,
    /// Inspect X11 / XWayland automation tools (`xdotool`, `wmctrl`, `xclip`, `scrot`/`import`).
    X11,
    /// Inspect native Wayland tools & permissions (`ydotool`, `/dev/uinput`, `wl-clipboard`).
    Wayland,
    /// Inspect isolated virtual desktop tools (`Xvfb`, `xvfb-run` + X11 stack).
    VirtualX11,
    /// Inspect OCR fallback tools (`tesseract`, `chi_sim`/`eng`, optional `onnxruntime` + PP-OCR).
    Ocr,
    /// Inspect every profile (`X11` + `Wayland` + `VirtualX11` + `Ocr`).
    All,
}

impl EnvProfile {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => Ok(Self::Auto),
            "x11" | "xorg" => Ok(Self::X11),
            "wayland" => Ok(Self::Wayland),
            "xvfb" | "virtual" | "virtual-x11" | "virtual_x11" => Ok(Self::VirtualX11),
            "ocr" | "vision" => Ok(Self::Ocr),
            "all" | "full" => Ok(Self::All),
            other => Err(UiError::Internal(format!(
                "invalid profile '{other}'; expected auto, x11, wayland, xvfb, ocr, or all"
            ))
            .into()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageManager {
    Apt,
    Dnf,
    Pacman,
    Zypper,
    Unknown,
}

impl PackageManager {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Apt => "apt-get",
            Self::Dnf => "dnf",
            Self::Pacman => "pacman",
            Self::Zypper => "zypper",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Ok,
    Missing,
    PermissionDenied,
    Degraded,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckItem {
    pub id: String,
    pub category: String,
    pub title: String,
    pub status: CheckStatus,
    pub detail: String,
    pub required_for: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RemediationAction {
    InstallPackages {
        manager: PackageManager,
        packages: Vec<String>,
        command: String,
    },
    ConfigureUinput {
        user: String,
        rule_path: String,
        rule_content: String,
        commands: Vec<String>,
    },
    StartService {
        name: String,
        command: String,
    },
    ManualHint {
        title: String,
        detail: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub profile: EnvProfile,
    pub diagnostics: PlatformDiagnostics,
    pub distro: String,
    pub package_manager: PackageManager,
    pub current_user: String,
    pub in_input_group: bool,
    pub uinput_exists: bool,
    pub udev_rule_present: bool,
    pub atspi_ok: bool,
    pub atspi_detail: String,
    pub tesseract_langs: Vec<String>,
    pub checks: Vec<CheckItem>,
    pub actions: Vec<RemediationAction>,
}

impl DoctorReport {
    /// True if all checked items for the selected profile are `Ok` (optional hints excluded).
    pub fn is_ready(&self) -> bool {
        !self
            .checks
            .iter()
            .any(|c| matches!(c.status, CheckStatus::Missing | CheckStatus::PermissionDenied))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplyOutcome {
    pub executed: bool,
    pub elevation_method: Option<String>,
    pub privileged_script: Option<String>,
    pub started_ydotoold: bool,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd)]
enum LogicalPackage {
    Xdotool,
    Wmctrl,
    Xclip,
    Scrot,
    Xvfb,
    Ydotool,
    WlClipboard,
    Tesseract,
    TesseractChiSim,
}

const UDEV_RULE_PATH: &str = "/etc/udev/rules.d/80-automata-uinput.rules";
const UDEV_RULE_CONTENT: &str =
    r#"KERNEL=="uinput", MODE="0660", GROUP="input", OPTIONS+="static_node=uinput""#;

/// Inspect Linux desktop automation readiness for the requested [`EnvProfile`].
pub fn inspect_environment(profile: EnvProfile) -> DoctorReport {
    let diagnostics = platform_diagnostics();
    let distro = read_distro_pretty_name().unwrap_or_else(|| "Linux".to_string());
    let package_manager = detect_package_manager();
    let current_user = current_username();
    let in_input_group = user_in_input_group(&current_user);
    let uinput_exists = Path::new("/dev/uinput").exists();
    let udev_rule_present = has_uinput_udev_rule();
    let (atspi_ok, atspi_detail) = check_atspi_bus();
    let tesseract_langs = list_tesseract_langs();

    let active_display = resolve_display_backend().unwrap_or(DisplayBackend::X11);
    let check_x11 = matches!(profile, EnvProfile::X11 | EnvProfile::VirtualX11 | EnvProfile::All)
        || (profile == EnvProfile::Auto && active_display == DisplayBackend::X11);
    let check_wayland = matches!(profile, EnvProfile::Wayland | EnvProfile::All)
        || (profile == EnvProfile::Auto && active_display == DisplayBackend::Wayland);
    let check_xvfb = matches!(profile, EnvProfile::VirtualX11 | EnvProfile::All);
    let check_ocr = matches!(profile, EnvProfile::Ocr | EnvProfile::Auto | EnvProfile::All);

    let mut checks = Vec::new();
    let mut needed_pkgs: BTreeSet<LogicalPackage> = BTreeSet::new();

    // 1. AT-SPI2 bus & accessibility environment
    checks.push(CheckItem {
        id: "atspi_bus".into(),
        category: "a11y".into(),
        title: "AT-SPI2 无障碍总线连通性".into(),
        status: if atspi_ok {
            CheckStatus::Ok
        } else {
            CheckStatus::Degraded
        },
        detail: atspi_detail.clone(),
        required_for: vec!["x11".into(), "wayland".into(), "xvfb".into()],
    });
    let (a11y_env_checks, mut a11y_env_hints) =
        inspect_a11y_env_with(check_xvfb, |k| std::env::var(k).ok());
    checks.extend(a11y_env_checks);

    let (aa_check, aa_hint) =
        inspect_apparmor_label_check(crate::a11y::current_apparmor_label().as_deref());
    checks.push(aa_check);
    if let Some(hint) = aa_hint {
        a11y_env_hints.push(hint);
    }

    let toolkit_probe = crate::diagnostics::cache::probe_atspi_toolkit_enabled();
    checks.push(CheckItem {
        id: "atspi_toolkit_gsettings".into(),
        category: "a11y".into(),
        title: "AT-SPI2 工具包 GSettings (兼容 2.62+)".into(),
        status: if !toolkit_probe.schema_found || toolkit_probe.enabled {
            CheckStatus::Ok
        } else {
            CheckStatus::Degraded
        },
        detail: toolkit_probe.detail,
        required_for: vec!["x11".into(), "wayland".into()],
    });

    let comp_caps = crate::compositor::active_backend().capabilities();
    checks.push(CheckItem {
        id: "compositor_backend".into(),
        category: "window".into(),
        title: format!("Compositor 窗口后端 ({})", comp_caps.kind.as_str()),
        status: if comp_caps.available {
            CheckStatus::Ok
        } else {
            CheckStatus::Degraded
        },
        detail: comp_caps.detail,
        required_for: vec!["x11".into(), "wayland".into()],
    });

    if let Some(xkb) = crate::input::xkb::inspect_xkb_layout() {
        checks.push(CheckItem {
            id: "xkb_keyboard_layout".into(),
            category: "input".into(),
            title: "XKB 键盘布局状态".into(),
            status: if xkb.has_non_us_primary {
                CheckStatus::Degraded
            } else {
                CheckStatus::Ok
            },
            detail: format!(
                "layouts=[{}]{}",
                xkb.layouts.join(","),
                if xkb.has_non_us_primary {
                    " (primary layout is non-Latin/non-US; ASCII shortcut keysyms may need layout switch)"
                } else {
                    ""
                }
            ),
            required_for: vec!["x11".into()],
        });
    }

    // 2. X11 / XWayland tools
    if check_x11 {
        check_binary(
            &mut checks,
            &mut needed_pkgs,
            "xdotool",
            "input",
            "xdotool (X11/XWayland/Xvfb 键鼠与窗口探测)",
            LogicalPackage::Xdotool,
            &["x11", "xvfb"],
        );
        check_binary(
            &mut checks,
            &mut needed_pkgs,
            "wmctrl",
            "window",
            "wmctrl (X11/XWayland 窗口前置与状态控制)",
            LogicalPackage::Wmctrl,
            &["x11", "xvfb"],
        );
        check_any_binary(
            &mut checks,
            &mut needed_pkgs,
            "x11_clipboard",
            "clipboard",
            "X11 剪贴板工具 (xclip 或 xsel)",
            &["xclip", "xsel"],
            LogicalPackage::Xclip,
            &["x11", "xvfb"],
        );
        check_any_binary(
            &mut checks,
            &mut needed_pkgs,
            "x11_screenshot",
            "screenshot",
            "X11 截图工具 (scrot 或 ImageMagick import)",
            &["scrot", "import"],
            LogicalPackage::Scrot,
            &["x11", "xvfb"],
        );
    }

    // 3. Virtual X11 (Xvfb)
    if check_xvfb {
        check_binary(
            &mut checks,
            &mut needed_pkgs,
            "Xvfb",
            "virtual_desktop",
            "Xvfb (隔离虚拟 X11 显示服务器)",
            LogicalPackage::Xvfb,
            &["xvfb"],
        );
    }

    // 4. Wayland tools & uinput permissions
    if check_wayland {
        check_any_binary(
            &mut checks,
            &mut needed_pkgs,
            "ydotool",
            "input",
            "ydotool (原生 Wayland 内核级虚拟键鼠客户端)",
            &["ydotool"],
            LogicalPackage::Ydotool,
            &["wayland"],
        );
        check_binary(
            &mut checks,
            &mut needed_pkgs,
            "wl-copy",
            "clipboard",
            "wl-clipboard (wl-copy / wl-paste 原生 Wayland 剪贴板)",
            LogicalPackage::WlClipboard,
            &["wayland"],
        );
        checks.push(CheckItem {
            id: "uinput_device".into(),
            category: "permission".into(),
            title: "/dev/uinput 内核设备读写权限".into(),
            status: if !uinput_exists {
                CheckStatus::Missing
            } else if uinput_writable() {
                CheckStatus::Ok
            } else {
                CheckStatus::PermissionDenied
            },
            detail: if !uinput_exists {
                "/dev/uinput 不存在（需加载 uinput 内核模块）".into()
            } else if uinput_writable() {
                "/dev/uinput 当前用户可读写".into()
            } else {
                format!(
                    "/dev/uinput 不可写 (user={current_user}, in_input_group={in_input_group}, udev_rule={udev_rule_present})"
                )
            },
            required_for: vec!["wayland".into()],
        });

        let ydotool_installed = command_exists("ydotool");
        checks.push(CheckItem {
            id: "ydotoold".into(),
            category: "service".into(),
            title: "ydotoold 守护进程 / socket 就绪".into(),
            status: if ydotoold_running() {
                CheckStatus::Ok
            } else if ydotool_installed && uinput_writable() {
                CheckStatus::Degraded
            } else {
                CheckStatus::Missing
            },
            detail: if ydotoold_running() {
                "ydotoold 正在运行或 /dev/uinput 已就绪".into()
            } else {
                "ydotoold 未运行（Wayland 原生指针注入需要启动 ydotoold 或授权 /dev/uinput）".into()
            },
            required_for: vec!["wayland".into()],
        });
    }

    // 5. OCR fallback readiness
    if check_ocr {
        check_binary(
            &mut checks,
            &mut needed_pkgs,
            "tesseract",
            "ocr",
            "tesseract OCR 引擎",
            LogicalPackage::Tesseract,
            &["ocr"],
        );
        let has_eng = tesseract_langs.iter().any(|l| l == "eng");
        let has_chi = tesseract_langs.iter().any(|l| l == "chi_sim");
        let lang_status = if !command_exists("tesseract") {
            CheckStatus::Missing
        } else if has_eng && has_chi {
            CheckStatus::Ok
        } else {
            CheckStatus::Degraded
        };
        if command_exists("tesseract") && !has_chi {
            needed_pkgs.insert(LogicalPackage::TesseractChiSim);
        }
        checks.push(CheckItem {
            id: "tesseract_langs".into(),
            category: "ocr".into(),
            title: "Tesseract 中英文语言包 (eng + chi_sim)".into(),
            status: lang_status,
            detail: if tesseract_langs.is_empty() {
                "未检测到可用 tessdata 语言包".into()
            } else {
                format!("已安装语言: {}", tesseract_langs.join(", "))
            },
            required_for: vec!["ocr".into()],
        });

        let ort_dylib = find_ort_dylib();
        let ort_models = find_ocr_model_dir();
        checks.push(CheckItem {
            id: "ort_ppocr".into(),
            category: "ocr".into(),
            title: "ONNX Runtime + PP-OCRv4/v5 模型（可选高精度加速）".into(),
            status: if ort_dylib.is_some() && ort_models.is_some() {
                CheckStatus::Ok
            } else {
                CheckStatus::Degraded
            },
            detail: format!(
                "dylib={}, models={}",
                ort_dylib
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "<not found>".into()),
                ort_models
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "<not configured>".into())
            ),
            required_for: vec![],
        });
    }

    // Build concrete remediation plan
    let mut actions = Vec::new();
    actions.append(&mut a11y_env_hints);
    if !needed_pkgs.is_empty() {
        let mut pkgs: Vec<String> = needed_pkgs
            .iter()
            .filter_map(|p| package_for(package_manager, *p))
            .map(str::to_string)
            .collect();
        pkgs.sort();
        pkgs.dedup();
        if !pkgs.is_empty() {
            let cmd = format_install_command(package_manager, &pkgs);
            actions.push(RemediationAction::InstallPackages {
                manager: package_manager,
                packages: pkgs,
                command: cmd,
            });
        }
    }

    if check_wayland && (!uinput_writable() || !in_input_group || !udev_rule_present) {
        let cmds = vec![
            format!(
                "printf '%s\\n' '{}' > {}",
                UDEV_RULE_CONTENT, UDEV_RULE_PATH
            ),
            "groupadd -f input".to_string(),
            format!("usermod -aG input {current_user}"),
            "modprobe uinput || true".to_string(),
            "udevadm control --reload-rules && udevadm trigger --name-match=uinput".to_string(),
            "chgrp input /dev/uinput && chmod 0660 /dev/uinput".to_string(),
            format!(
                "command -v setfacl >/dev/null 2>&1 && setfacl -m u:{current_user}:rw /dev/uinput || true"
            ),
        ];
        actions.push(RemediationAction::ConfigureUinput {
            user: current_user.clone(),
            rule_path: UDEV_RULE_PATH.to_string(),
            rule_content: UDEV_RULE_CONTENT.to_string(),
            commands: cmds,
        });
    }

    if check_wayland && !ydotoold_running() {
        actions.push(RemediationAction::StartService {
            name: "ydotoold".to_string(),
            command: "systemctl --user enable --now ydotool || nohup ydotoold >/tmp/ydotoold.log 2>&1 &".to_string(),
        });
    }

    if check_ocr && find_ocr_model_dir().is_none() {
        actions.push(RemediationAction::ManualHint {
            title: "可选：配置 PP-OCRv4/v5 ONNX 高精度中文 OCR".into(),
            detail: "如需比 Tesseract 更高的中文 UI 识别率，可下载 det.onnx/rec.onnx/keys.txt 到 ~/.local/share/ui-automata/models/ppocr 或设置 AUTOMATA_OCR_MODEL_DIR".into(),
        });
    }

    DoctorReport {
        profile,
        diagnostics,
        distro,
        package_manager,
        current_user,
        in_input_group,
        uinput_exists,
        udev_rule_present,
        atspi_ok,
        atspi_detail,
        tesseract_langs,
        checks,
        actions,
    }
}

/// Build an idempotent `sh -e` root remediation script from [`RemediationAction`]s, if any root step exists.
pub fn build_privileged_script(actions: &[RemediationAction]) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    for act in actions {
        match act {
            RemediationAction::InstallPackages {
                manager, command, ..
            } => {
                if *manager != PackageManager::Unknown {
                    lines.push(command.clone());
                }
            }
            RemediationAction::ConfigureUinput { commands, .. } => {
                lines.extend(commands.iter().cloned());
            }
            _ => {}
        }
    }
    if lines.is_empty() {
        None
    } else {
        Some(format!("set -e\n{}", lines.join("\n")))
    }
}

/// Execute automated package installation and `/dev/uinput` permission setup using
/// `sudo` (when running in a TTY) or Polkit `pkexec` (on desktop without a TTY).
pub fn apply_remediation(report: &DoctorReport, dry_run: bool) -> Result<ApplyOutcome> {
    let script = build_privileged_script(&report.actions);
    let wants_ydotoold = report
        .actions
        .iter()
        .any(|a| matches!(a, RemediationAction::StartService { name, .. } if name == "ydotoold"));

    if dry_run {
        return Ok(ApplyOutcome {
            executed: false,
            elevation_method: None,
            privileged_script: script,
            started_ydotoold: false,
            notes: vec!["dry-run mode: no system changes were executed".into()],
        });
    }

    let mut notes = Vec::new();
    let mut elevation_method = None;

    if let Some(ref body) = script {
        let method = if std::io::stdin().is_terminal() && command_exists("sudo") {
            "sudo"
        } else if command_exists("pkexec") {
            "pkexec"
        } else if command_exists("sudo") {
            "sudo"
        } else {
            return Err(UiError::Platform(
                "neither `sudo` nor `pkexec` is available to perform privileged setup".into(),
            )
            .into());
        };
        elevation_method = Some(method.to_string());

        let status = Command::new(method)
            .args(["sh", "-ec", body])
            .status()
            .map_err(|e| UiError::Platform(format!("failed to invoke {method}: {e}")))?;

        if !status.success() {
            return Err(UiError::Platform(format!(
                "privileged setup via {method} failed with exit status {status}"
            ))
            .into());
        }
        notes.push(format!("privileged installation/authorization completed via {method}"));

        if !report.in_input_group {
            notes.push(format!(
                "added user '{}' to group `input`; start a new login session or run `newgrp input` if current shell cannot yet open /dev/uinput",
                report.current_user
            ));
        }
    } else {
        notes.push("no privileged installation or permission changes required".into());
    }

    let mut started_ydotoold = false;
    if wants_ydotoold && command_exists("ydotoold") {
        let systemd_ok = Command::new("systemctl")
            .args(["--user", "enable", "--now", "ydotool"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if systemd_ok {
            started_ydotoold = true;
            notes.push("started user systemd service `ydotool`".into());
        } else if uinput_writable() {
            if let Ok(_child) = Command::new("ydotoold")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                started_ydotoold = true;
                notes.push("spawned background `ydotoold` daemon".into());
            }
        } else {
            notes.push(
                "`ydotoold` not started yet because current shell still lacks `input` group token; re-login or run `newgrp input` then start `ydotoold`".into(),
            );
        }
    }

    Ok(ApplyOutcome {
        executed: true,
        elevation_method,
        privileged_script: script,
        started_ydotoold,
        notes,
    })
}

fn check_binary(
    checks: &mut Vec<CheckItem>,
    needed_pkgs: &mut BTreeSet<LogicalPackage>,
    bin: &str,
    category: &str,
    title: &str,
    pkg: LogicalPackage,
    required_for: &[&str],
) {
    let ok = command_exists(bin);
    if !ok {
        needed_pkgs.insert(pkg);
    }
    checks.push(CheckItem {
        id: bin.to_ascii_lowercase(),
        category: category.into(),
        title: title.into(),
        status: if ok {
            CheckStatus::Ok
        } else {
            CheckStatus::Missing
        },
        detail: if ok {
            format!("已安装 `{bin}`")
        } else {
            format!("未在 PATH 中找到 `{bin}`")
        },
        required_for: required_for.iter().map(|s| (*s).to_string()).collect(),
    });
}

fn check_any_binary(
    checks: &mut Vec<CheckItem>,
    needed_pkgs: &mut BTreeSet<LogicalPackage>,
    id: &str,
    category: &str,
    title: &str,
    bins: &[&str],
    fallback_pkg: LogicalPackage,
    required_for: &[&str],
) {
    let found = bins.iter().copied().find(|b| command_exists(b));
    if found.is_none() {
        needed_pkgs.insert(fallback_pkg);
    }
    checks.push(CheckItem {
        id: id.into(),
        category: category.into(),
        title: title.into(),
        status: if found.is_some() {
            CheckStatus::Ok
        } else {
            CheckStatus::Missing
        },
        detail: match found {
            Some(b) => format!("已安装 `{b}`"),
            None => format!("未找到以下任一命令: {}", bins.join(", ")),
        },
        required_for: required_for.iter().map(|s| (*s).to_string()).collect(),
    });
}

fn check_atspi_bus() -> (bool, String) {
    let try_connect = || match A11yClient::shared() {
        Ok(client) => match client.root_applications() {
            Ok(apps) => Ok(format!(
                "AT-SPI2 已连通，检测到 {} 个可访问应用节点",
                apps.len()
            )),
            Err(e) => Err(format!("AT-SPI2 客户端已连接，但枚举应用失败: {e}")),
        },
        Err(e) => Err(format!("无法连接 AT-SPI2 总线: {e}")),
    };

    match try_connect() {
        Ok(msg) => (true, msg),
        Err(_) => {
            // Reset once in case at-spi-registryd restarted or the session bus recovered.
            A11yClient::reset_shared();
            match try_connect() {
                Ok(msg) => (true, msg),
                Err(msg) => (false, msg),
            }
        }
    }
}

fn inspect_a11y_env_with<F>(
    check_xvfb: bool,
    get_env: F,
) -> (Vec<CheckItem>, Vec<RemediationAction>)
where
    F: Fn(&str) -> Option<String>,
{
    let mut checks = Vec::new();
    let mut hints = Vec::new();

    let dbus_addr = get_env("DBUS_SESSION_BUS_ADDRESS")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let explicit_atspi = get_env("AT_SPI_BUS_ADDRESS")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let (dbus_status, dbus_detail) = match (&dbus_addr, &explicit_atspi) {
        (None, _) => (
            CheckStatus::Degraded,
            "DBUS_SESSION_BUS_ADDRESS 未设置；无头/Xvfb 环境建议启动 dbus-run-session 以托管 org.a11y.Bus".into(),
        ),
        (Some(dbus), Some(atspi)) => (
            CheckStatus::Degraded,
            format!(
                "DBUS_SESSION_BUS_ADDRESS={dbus}，但同时设置了 AT_SPI_BUS_ADDRESS={atspi}（若指向固定 bus_* socket，registryd 重启后易失效）"
            ),
        ),
        (Some(dbus), None) => (
            CheckStatus::Ok,
            format!("使用 session bus ({dbus}) 托管 org.a11y.Bus"),
        ),
    };
    checks.push(CheckItem {
        id: "a11y_dbus_session".into(),
        category: "a11y".into(),
        title: "D-Bus Session Bus 与 AT-SPI2 托管方式".into(),
        status: dbus_status,
        detail: dbus_detail,
        required_for: vec!["x11".into(), "wayland".into(), "xvfb".into()],
    });
    if explicit_atspi.is_some() {
        hints.push(RemediationAction::ManualHint {
            title: "建议移除硬编码的 AT_SPI_BUS_ADDRESS".into(),
            detail: "避免硬编码 ~/.cache/at-spi/bus_*；让进程直接通过 DBUS_SESSION_BUS_ADDRESS 连接 org.a11y.Bus 具备更好的自愈稳定性。".into(),
        });
    }

    let qt_a11y = get_env("QT_ACCESSIBILITY");
    let qt_always = get_env("QT_LINUX_ACCESSIBILITY_ALWAYS_ON");
    let no_at_bridge = get_env("NO_AT_BRIDGE");
    let qt_ok = qt_a11y.as_deref() == Some("1") || qt_always.as_deref() == Some("1");
    let bridge_muted = no_at_bridge.as_deref() == Some("1");

    let (tk_status, tk_detail) = if !qt_ok || bridge_muted {
        (
            CheckStatus::Degraded,
            format!(
                "QT_ACCESSIBILITY={}, QT_LINUX_ACCESSIBILITY_ALWAYS_ON={}, NO_AT_BRIDGE={}（open_application 启动目标时会自动注入 QT_ACCESSIBILITY=1 与 NO_AT_BRIDGE=0，但外部已启动的 Qt/GTK 应用需自行导出）",
                qt_a11y.as_deref().unwrap_or("<unset>"),
                qt_always.as_deref().unwrap_or("<unset>"),
                no_at_bridge.as_deref().unwrap_or("<unset>"),
            ),
        )
    } else {
        (
            CheckStatus::Ok,
            format!(
                "QT_ACCESSIBILITY={}, NO_AT_BRIDGE={}",
                qt_a11y.as_deref().unwrap_or("<unset>"),
                no_at_bridge.as_deref().unwrap_or("<unset>"),
            ),
        )
    };
    checks.push(CheckItem {
        id: "a11y_toolkit_env".into(),
        category: "a11y".into(),
        title: "Qt / GTK 无障碍环境变量 (QT_ACCESSIBILITY / NO_AT_BRIDGE)".into(),
        status: tk_status,
        detail: tk_detail,
        required_for: vec![],
    });

    if check_xvfb {
        let qt_sw = get_env("QT_QUICK_BACKEND").as_deref() == Some("software");
        let gl_sw = get_env("LIBGL_ALWAYS_SOFTWARE").as_deref() == Some("1");
        checks.push(CheckItem {
            id: "xvfb_software_render".into(),
            category: "virtual_desktop".into(),
            title: "Xvfb 无 GPU 软件渲染环境变量 (可选)".into(),
            status: if qt_sw && gl_sw {
                CheckStatus::Ok
            } else {
                CheckStatus::Degraded
            },
            detail: format!(
                "QT_QUICK_BACKEND={}, LIBGL_ALWAYS_SOFTWARE={}",
                get_env("QT_QUICK_BACKEND").as_deref().unwrap_or("<unset>"),
                get_env("LIBGL_ALWAYS_SOFTWARE").as_deref().unwrap_or("<unset>"),
            ),
            required_for: vec![],
        });
        if !(qt_sw && gl_sw) {
            hints.push(RemediationAction::ManualHint {
                title: "无头 Xvfb 跑 Qt/微信建议设置软件渲染变量".into(),
                detail: "无 GPU/DRI3 环境下可导出 QT_QUICK_BACKEND=software QMLSCENE_DEVICE=softwarecontext LIBGL_ALWAYS_SOFTWARE=1".into(),
            });
        }
    }

    (checks, hints)
}

fn inspect_apparmor_label_check(label: Option<&str>) -> (CheckItem, Option<RemediationAction>) {
    let trimmed = label.map(str::trim).filter(|s| !s.is_empty());
    let is_unconfined = trimmed
        .map(|l| l == "unconfined" || l.starts_with("unconfined "))
        .unwrap_or(true);

    if is_unconfined {
        return (
            CheckItem {
                id: "a11y_apparmor_confinement".into(),
                category: "a11y".into(),
                title: "进程 AppArmor 标签 (Snap AT-SPI2 互操作)".into(),
                status: CheckStatus::Ok,
                detail: format!("当前进程 AppArmor 标签: {}", trimmed.unwrap_or("<unavailable>")),
                required_for: vec![],
            },
            None,
        );
    }

    let label_str = trimmed.unwrap();
    (
        CheckItem {
            id: "a11y_apparmor_confinement".into(),
            category: "a11y".into(),
            title: "进程 AppArmor 标签 (Snap AT-SPI2 互操作)".into(),
            status: CheckStatus::Degraded,
            detail: format!(
                "当前进程带有 AppArmor 标签 '{label_str}'（非 unconfined）：Snap 应用仅允许 label=unconfined 访问其 AT-SPI2 D-Bus，将被跳过"
            ),
            required_for: vec![],
        },
        Some(RemediationAction::ManualHint {
            title: "使用 `aa-exec -p unconfined` 启动 MCP 以访问 Snap 应用 AT-SPI2".into(),
            detail: "当由带有独立 profile 的宿主（如 ai.opencode）拉起时，可将命令包装为 `aa-exec -p unconfined -- automata-agent --stdio`，或在外部终端运行 `--port 3742` HTTP 模式。".into(),
        }),
    )
}

fn detect_package_manager() -> PackageManager {
    detect_package_manager_from(
        command_exists("apt-get"),
        command_exists("dnf"),
        command_exists("pacman"),
        command_exists("zypper"),
    )
}

pub fn detect_package_manager_from(
    has_apt: bool,
    has_dnf: bool,
    has_pacman: bool,
    has_zypper: bool,
) -> PackageManager {
    if has_apt {
        PackageManager::Apt
    } else if has_dnf {
        PackageManager::Dnf
    } else if has_pacman {
        PackageManager::Pacman
    } else if has_zypper {
        PackageManager::Zypper
    } else {
        PackageManager::Unknown
    }
}

fn package_for(manager: PackageManager, pkg: LogicalPackage) -> Option<&'static str> {
    match (manager, pkg) {
        (PackageManager::Apt, LogicalPackage::Xdotool) => Some("xdotool"),
        (PackageManager::Apt, LogicalPackage::Wmctrl) => Some("wmctrl"),
        (PackageManager::Apt, LogicalPackage::Xclip) => Some("xclip"),
        (PackageManager::Apt, LogicalPackage::Scrot) => Some("scrot"),
        (PackageManager::Apt, LogicalPackage::Xvfb) => Some("xvfb"),
        (PackageManager::Apt, LogicalPackage::Ydotool) => Some("ydotool"),
        (PackageManager::Apt, LogicalPackage::WlClipboard) => Some("wl-clipboard"),
        (PackageManager::Apt, LogicalPackage::Tesseract) => Some("tesseract-ocr"),
        (PackageManager::Apt, LogicalPackage::TesseractChiSim) => Some("tesseract-ocr-chi-sim"),

        (PackageManager::Dnf, LogicalPackage::Xdotool) => Some("xdotool"),
        (PackageManager::Dnf, LogicalPackage::Wmctrl) => Some("wmctrl"),
        (PackageManager::Dnf, LogicalPackage::Xclip) => Some("xclip"),
        (PackageManager::Dnf, LogicalPackage::Scrot) => Some("scrot"),
        (PackageManager::Dnf, LogicalPackage::Xvfb) => Some("xorg-x11-server-Xvfb"),
        (PackageManager::Dnf, LogicalPackage::Ydotool) => Some("ydotool"),
        (PackageManager::Dnf, LogicalPackage::WlClipboard) => Some("wl-clipboard"),
        (PackageManager::Dnf, LogicalPackage::Tesseract) => Some("tesseract"),
        (PackageManager::Dnf, LogicalPackage::TesseractChiSim) => Some("tesseract-langpack-chi_sim"),

        (PackageManager::Pacman, LogicalPackage::Xdotool) => Some("xdotool"),
        (PackageManager::Pacman, LogicalPackage::Wmctrl) => Some("wmctrl"),
        (PackageManager::Pacman, LogicalPackage::Xclip) => Some("xclip"),
        (PackageManager::Pacman, LogicalPackage::Scrot) => Some("scrot"),
        (PackageManager::Pacman, LogicalPackage::Xvfb) => Some("xorg-server-xvfb"),
        (PackageManager::Pacman, LogicalPackage::Ydotool) => Some("ydotool"),
        (PackageManager::Pacman, LogicalPackage::WlClipboard) => Some("wl-clipboard"),
        (PackageManager::Pacman, LogicalPackage::Tesseract) => Some("tesseract"),
        (PackageManager::Pacman, LogicalPackage::TesseractChiSim) => {
            Some("tesseract-data-chi_sim")
        }

        (PackageManager::Zypper, LogicalPackage::Xdotool) => Some("xdotool"),
        (PackageManager::Zypper, LogicalPackage::Wmctrl) => Some("wmctrl"),
        (PackageManager::Zypper, LogicalPackage::Xclip) => Some("xclip"),
        (PackageManager::Zypper, LogicalPackage::Scrot) => Some("scrot"),
        (PackageManager::Zypper, LogicalPackage::Xvfb) => Some("xorg-x11-server-Xvfb"),
        (PackageManager::Zypper, LogicalPackage::Ydotool) => Some("ydotool"),
        (PackageManager::Zypper, LogicalPackage::WlClipboard) => Some("wl-clipboard"),
        (PackageManager::Zypper, LogicalPackage::Tesseract) => Some("tesseract-ocr"),
        (PackageManager::Zypper, LogicalPackage::TesseractChiSim) => {
            Some("tesseract-ocr-traineddata-chinese_simplified")
        }

        (PackageManager::Unknown, _) => None,
    }
}

fn format_install_command(manager: PackageManager, packages: &[String]) -> String {
    let joined = packages.join(" ");
    match manager {
        PackageManager::Apt => {
            format!("DEBIAN_FRONTEND=noninteractive apt-get update && apt-get install -y {joined}")
        }
        PackageManager::Dnf => format!("dnf install -y {joined}"),
        PackageManager::Pacman => format!("pacman -Sy --noconfirm {joined}"),
        PackageManager::Zypper => format!("zypper --non-interactive install {joined}"),
        PackageManager::Unknown => format!("# install manually: {joined}"),
    }
}

fn read_distro_pretty_name() -> Option<String> {
    let raw = std::fs::read_to_string("/etc/os-release").ok()?;
    parse_os_release_pretty_name(&raw)
}

pub fn parse_os_release_pretty_name(raw: &str) -> Option<String> {
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("PRETTY_NAME=") {
            return Some(rest.trim().trim_matches('"').to_string());
        }
    }
    None
}

fn current_username() -> String {
    if let Ok(u) = std::env::var("USER") {
        if !u.trim().is_empty() {
            return u;
        }
    }
    Command::new("id")
        .arg("-un")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "root".into())
}

fn user_in_input_group(user: &str) -> bool {
    let out = Command::new("id").args(["-nG", user]).output();
    match out {
        Ok(o) if o.status.success() => {
            parse_id_groups(&String::from_utf8_lossy(&o.stdout)).contains("input")
        }
        _ => false,
    }
}

pub fn parse_id_groups(raw: &str) -> BTreeSet<String> {
    raw.split_whitespace().map(str::to_string).collect()
}

fn has_uinput_udev_rule() -> bool {
    for dir in ["/etc/udev/rules.d", "/lib/udev/rules.d", "/usr/lib/udev/rules.d"] {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path: PathBuf = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("rules") {
                continue;
            }
            if let Ok(body) = std::fs::read_to_string(&path) {
                if body.contains("uinput") && !body.trim_start().starts_with('#') {
                    return true;
                }
            }
        }
    }
    false
}

fn list_tesseract_langs() -> Vec<String> {
    if !command_exists("tesseract") {
        return vec![];
    }
    let Ok(out) = Command::new("tesseract").arg("--list-langs").output() else {
        return vec![];
    };
    if !out.status.success() {
        return vec![];
    }
    parse_tesseract_langs_output(&String::from_utf8_lossy(&out.stdout))
}

pub fn parse_tesseract_langs_output(raw: &str) -> Vec<String> {
    raw.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("List of available languages"))
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_os_release_and_package_managers() {
        let sample = "NAME=\"Ubuntu\"\nPRETTY_NAME=\"Ubuntu 25.10\"\n";
        assert_eq!(
            parse_os_release_pretty_name(sample).as_deref(),
            Some("Ubuntu 25.10")
        );
        assert_eq!(
            detect_package_manager_from(true, false, false, false),
            PackageManager::Apt
        );
        assert_eq!(
            detect_package_manager_from(false, true, false, false),
            PackageManager::Dnf
        );
        assert_eq!(
            detect_package_manager_from(false, false, true, false),
            PackageManager::Pacman
        );
        assert_eq!(
            detect_package_manager_from(false, false, false, true),
            PackageManager::Zypper
        );
    }

    #[test]
    fn parses_groups_and_tesseract_langs() {
        let groups = parse_id_groups("ivrty adm sudo input plugdev");
        assert!(groups.contains("input"));
        assert!(!groups.contains("wheel"));

        let langs = parse_tesseract_langs_output(
            "List of available languages in \"/usr/share/tesseract-ocr/5/tessdata/\" (3):\nchi_sim\neng\nosd\n",
        );
        assert_eq!(langs, vec!["chi_sim", "eng", "osd"]);
    }

    #[test]
    fn builds_safe_idempotent_privileged_script() {
        let actions = vec![
            RemediationAction::InstallPackages {
                manager: PackageManager::Apt,
                packages: vec!["ydotool".into()],
                command: "apt-get update && apt-get install -y ydotool".into(),
            },
            RemediationAction::ConfigureUinput {
                user: "alice".into(),
                rule_path: UDEV_RULE_PATH.into(),
                rule_content: UDEV_RULE_CONTENT.into(),
                commands: vec![
                    format!("printf '%s\\n' '{UDEV_RULE_CONTENT}' > {UDEV_RULE_PATH}"),
                    "usermod -aG input alice".into(),
                    "udevadm control --reload-rules && udevadm trigger --name-match=uinput".into(),
                ],
            },
        ];
        let script = build_privileged_script(&actions).expect("expected script");
        assert!(script.starts_with("set -e\n"));
        assert!(script.contains("apt-get install -y ydotool"));
        assert!(script.contains("MODE=\"0660\", GROUP=\"input\""));
        assert!(script.contains("usermod -aG input alice"));
        assert!(!script.contains("0666"));
    }

    #[test]
    fn inspect_and_dry_run_work_without_side_effects() {
        let report = inspect_environment(EnvProfile::Wayland);
        assert!(!report.checks.is_empty());
        let outcome = apply_remediation(&report, true).unwrap();
        assert!(!outcome.executed);
    }

    #[test]
    fn a11y_env_flags_explicit_atspi_socket_and_missing_qt_env() {
        let env: std::collections::HashMap<&str, &str> = [
            ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1000/bus"),
            ("AT_SPI_BUS_ADDRESS", "unix:path=/tmp/at-spi/bus_99"),
            ("NO_AT_BRIDGE", "1"),
        ]
        .into_iter()
        .collect();
        let (checks, hints) = inspect_a11y_env_with(true, |k| env.get(k).map(|v| (*v).to_string()));
        assert_eq!(checks.len(), 3);
        assert!(checks.iter().all(|c| c.status == CheckStatus::Degraded));
        assert_eq!(hints.len(), 2);
    }

    #[test]
    fn a11y_env_reports_ok_when_session_and_toolkit_env_clean() {
        let env: std::collections::HashMap<&str, &str> = [
            ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1000/bus"),
            ("QT_ACCESSIBILITY", "1"),
            ("NO_AT_BRIDGE", "0"),
            ("QT_QUICK_BACKEND", "software"),
            ("LIBGL_ALWAYS_SOFTWARE", "1"),
        ]
        .into_iter()
        .collect();
        let (checks, hints) = inspect_a11y_env_with(true, |k| env.get(k).map(|v| (*v).to_string()));
        assert_eq!(checks.len(), 3);
        assert!(checks.iter().all(|c| c.status == CheckStatus::Ok));
        assert!(hints.is_empty());
    }

    #[test]
    fn apparmor_label_check_flags_non_unconfined_profile_names() {
        let (ok_item, ok_hint) = inspect_apparmor_label_check(Some("unconfined"));
        assert_eq!(ok_item.status, CheckStatus::Ok);
        assert!(ok_hint.is_none());

        let (deg_item, deg_hint) =
            inspect_apparmor_label_check(Some("ai.opencode (unconfined)"));
        assert_eq!(deg_item.status, CheckStatus::Degraded);
        assert!(deg_hint.is_some());
    }
}
