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

# 3. release 无签名（可选传入版本号如: just release 0.1.0）
release ver="":
    .\scripts\package-appx.ps1 -Profile release -Version "{{ver}}"

# 运行工作区全部单元测试
test:
    cargo test --workspace
