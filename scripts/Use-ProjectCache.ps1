param([switch]$Report)

$ErrorActionPreference = 'Stop'
$taskCacheRoot = Split-Path -Parent $PSScriptRoot
$taskCachePaths = [ordered]@{
    CARGO_HOME = '.devtools/cargo'
    RUSTUP_HOME = '.devtools/rustup'
    CARGO_TARGET_DIR = 'src-tauri/target'
    TEMP = '.devtools/tmp'
    TMP = '.devtools/tmp'
    npm_config_cache = '.devtools/npm-cache'
    PNPM_HOME = '.devtools/pnpm-home'
    COREPACK_HOME = '.devtools/corepack'
    PIP_CACHE_DIR = '.devtools/pip-cache'
    UV_CACHE_DIR = '.devtools/uv-cache'
    XDG_CACHE_HOME = '.devtools/cache'
}
foreach ($taskCacheName in $taskCachePaths.Keys) {
    $taskCachePath = [IO.Path]::GetFullPath((Join-Path $taskCacheRoot $taskCachePaths[$taskCacheName]))
    New-Item -ItemType Directory -Path $taskCachePath -Force | Out-Null
    [Environment]::SetEnvironmentVariable($taskCacheName, $taskCachePath, 'Process')
}
if ($Report) {
    $taskCachePaths.Keys | ForEach-Object {
        [pscustomobject]@{ Variable = $_; Path = [Environment]::GetEnvironmentVariable($_, 'Process') }
    }
}
