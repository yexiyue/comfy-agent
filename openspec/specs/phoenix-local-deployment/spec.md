# phoenix-local-deployment Specification

## Purpose

提供可重复启动的本地 Phoenix 环境，用于保存和查看 Agent 轨迹、数据集及评测实验。通过明确的网络边界、持久化和隐私默认值，使观测数据保留在用户自己的运行环境。

## Requirements

### Requirement: Reproducible local service
部署配置 SHALL 固定已验证的 Phoenix 镜像版本，并仅在本机回环地址提供 UI 与 OTLP HTTP 接收入口。

#### Scenario: Start local Phoenix
- **WHEN** 容器引擎可用且执行文档中的启动命令
- **THEN** UI 可从 `http://localhost:6006` 访问，`/v1/traces` 可接收轨迹，端口不绑定所有网卡

### Requirement: Persistent storage
部署 SHALL 持久化轨迹、数据集和实验，普通停止或重建容器 MUST NOT 删除数据。

#### Scenario: Recreate container
- **WHEN** 已写入轨迹和实验后停止并重建容器而未显式删除卷
- **THEN** 已有数据仍能查询，文档明确区分停止服务与删除数据

### Requirement: Local privacy defaults
默认部署 SHALL 关闭 Phoenix 遥测及自动加载外部资源，并且评测默认 MUST NOT 使用云端评分器。文档 SHALL 明确用户配置的远程模型仍接收模型请求。

#### Scenario: Default configuration
- **WHEN** 使用仓库提供的默认部署与评测配置
- **THEN** 无 Phoenix 遥测、自动外部资源加载或云端评分器调用，模型提供商的数据边界在文档中可见

### Requirement: Deployment readiness
部署文档 SHALL 提供引擎、服务和真实轨迹接收的验证步骤，并区分安装完成和服务可运行。

#### Scenario: Engine unavailable before reboot
- **WHEN** Docker CLI 已安装但容器引擎因待重启而不可用
- **THEN** 指引要求完成系统重启并检查引擎，不将安装成功标记为 Phoenix 验收通过
