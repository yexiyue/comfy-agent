param(
    [string]$BaseUrl = 'http://127.0.0.1:8188',
    [string]$WorkflowPath = 'workflows/sdxl_txt2img.api.json',
    [string]$OutputDir = 'outputs',
    [int]$TimeoutSeconds = 120
)

$ErrorActionPreference = 'Stop'
if ($TimeoutSeconds -le 0) { throw 'TimeoutSeconds 必须大于 0' }
$BaseUrl = $BaseUrl.TrimEnd('/')
New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
$workflow = Get-Content -LiteralPath $WorkflowPath -Raw -Encoding UTF8 | ConvertFrom-Json
if ($null -ne $workflow.nodes) { throw '这是编辑器格式，请重新导出 API 格式' }
if (@($workflow.PSObject.Properties).Count -eq 0) { throw '工作流不能为空' }
foreach ($node in $workflow.PSObject.Properties) {
    if (-not $node.Value.class_type -or $null -eq $node.Value.inputs) {
        throw "节点 $($node.Name) 缺少 class_type 或 inputs"
    }
}

$clientId = [guid]::NewGuid().ToString()
$body = @{ prompt = $workflow; client_id = $clientId } | ConvertTo-Json -Depth 100
$requestPath = Join-Path $OutputDir 'request.json'
[System.IO.File]::WriteAllText(
    [System.IO.Path]::GetFullPath($requestPath),
    $body,
    [System.Text.UTF8Encoding]::new($false)
)

$submitText = curl.exe --fail --silent --show-error --max-time 15 `
    -H 'Content-Type: application/json' `
    --data-binary "@$requestPath" "$BaseUrl/prompt"
if ($LASTEXITCODE -ne 0) { throw '提交失败；检查服务器日志与工作流参数，勿直接重复提交' }
$submitted = ($submitText -join "`n") | ConvertFrom-Json
if (-not $submitted.prompt_id) { throw '服务端没有返回 prompt_id' }
if ($submitted.node_errors -and @($submitted.node_errors.PSObject.Properties).Count -gt 0) {
    $submitted.node_errors | ConvertTo-Json -Depth 30 | Write-Warning
}
$promptId = [string]$submitted.prompt_id
Write-Host "任务已接收：$promptId；还未确认出图成功"

$deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
$job = $null
while ([DateTime]::UtcNow -lt $deadline) {
    $historyText = curl.exe --fail --silent --show-error --max-time 10 "$BaseUrl/history/$promptId"
    if ($LASTEXITCODE -ne 0) { throw '查询历史失败' }
    $history = ($historyText -join "`n") | ConvertFrom-Json
    $entry = $history.PSObject.Properties[$promptId]
    if ($null -ne $entry) {
        $candidate = $entry.Value
        if ($candidate.status.status_str -eq 'error') {
            throw ("任务执行失败：" + ($candidate.status | ConvertTo-Json -Depth 30 -Compress))
        }
        if ($candidate.status.completed -eq $true -and $candidate.status.status_str -eq 'success') {
            $job = $candidate
            break
        }
    }
    Start-Sleep -Seconds 1
}
if ($null -eq $job) {
    throw "等待超过 $TimeoutSeconds 秒；任务可能仍在运行，超时不会取消任务：$promptId"
}

$images = @(
    foreach ($output in $job.outputs.PSObject.Properties) {
        foreach ($image in $output.Value.images) {
            if ($null -ne $image) { $image }
        }
    }
)
if ($images.Count -eq 0) { throw '任务成功，但没有 images 输出；检查 SaveImage 节点' }
for ($i = 0; $i -lt $images.Count; $i++) {
    $image = $images[$i]
    $ext = [System.IO.Path]::GetExtension([string]$image.filename)
    if ([string]::IsNullOrEmpty($ext)) { $ext = '.png' }
    $localPath = Join-Path $OutputDir ("$promptId-$i$ext")
    curl.exe --fail --silent --show-error --max-time 30 --get "$BaseUrl/view" `
        --data-urlencode "filename=$($image.filename)" `
        --data-urlencode "subfolder=$($image.subfolder)" `
        --data-urlencode "type=$($image.type)" `
        --output $localPath
    if ($LASTEXITCODE -ne 0) { throw '下载图片失败' }
    Write-Host "已下载：$localPath"
}