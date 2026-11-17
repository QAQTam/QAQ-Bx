set shell := ["powershell.exe", "-NoLogo", "-ExecutionPolicy", "Bypass", "-Command"]

# 默认列出可用命令
default:
    @just --list

# 1. debug + 自签名（前 3 位取自 Cargo.toml，第 4 位按出包时序自动 +1）
dev:
    .\scripts\package-appx.ps1 -Profile debug -Sign -AutoBumpRevision

# 2. release + 自签名（可选传入版本号如: just release-sign 0.1.0）
release-sign ver="":
    .\scripts\package-appx.ps1 -Profile release -Sign -Version "{{ver}}"

# 3. release 无签名（微软商店上传包，可覆写 Partner Center 产品标识）
release ver="":
    .\scripts\package-appx.ps1 -Profile release -Version "{{ver}}"

# 4. Microsoft Store 打包入口（传入 Partner Center Product Identity 3 项 + 非零版本号）
store pkg="QAQ-Bx" pub="CN=QAQ" pub_name="QAQ" ver="0.1.0":
    .\scripts\package-appx.ps1 -Profile release -Version "{{ver}}" -PackageName "{{pkg}}" -Publisher "{{pub}}" -PublisherDisplayName "{{pub_name}}"

# 运行工作区全部单元测试
test:
    cargo test --workspace

# 运行自动化 UI 演示与逐场景截图（target/ui-demo/frames/）
demo:
    .\scripts\ui-demo.ps1

# 内部状态自测：不开窗口、直接派发 Message 断言（AGENTS.md #4 集成测试）
# 固定从 target/debug 启动:与打包应用共用同一 data 目录(settings 无 home_open_mode
# → 默认 FullPlayer),断言覆盖完整路由路径,且不触碰仓库根 data/ 的用户设置。
selftest scenario="all":
    cargo build -p qaq-bx-ui --features selftest
    cd .\target\debug; .\qaq-bx-ui.exe --selftest {{scenario}}

# 录制 60fps 宣传片 MP4（target/ui-demo/promo.mp4）
demo-record:
    .\scripts\ui-demo.ps1 -Record

promo-smoke:
    cd promo-video; bun run smoke

promo-render ratio="9:16":
    cd promo-video; bun run render -- --ratio={{ratio}} --fps=30

promo:
    .\scripts\ui-demo.ps1 -Record
    cd promo-video; bun run render -- --ratio=9:16 --fps=60
