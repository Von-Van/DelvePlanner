$ErrorActionPreference = "Stop"
$version = "0.32.0"
$digest = "56561a8f0a904483303c610e61af61c5a7b6f5496ce3707e207d25d4ff67b89e"
$tmp = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { $env:TEMP }
$archive = Join-Path $tmp "ollama-windows-amd64-$version.zip"
$url = "https://github.com/ollama/ollama/releases/download/v$version/ollama-windows-amd64.zip"
$destination = "src-tauri/resources/ollama/windows-x86_64"

Invoke-WebRequest -Uri $url -OutFile $archive
if ((Get-FileHash $archive -Algorithm SHA256).Hash.ToLowerInvariant() -ne $digest) {
  throw "Ollama runtime checksum mismatch"
}
New-Item -ItemType Directory -Force $destination | Out-Null
Expand-Archive -Path $archive -DestinationPath $destination -Force
if (-not (Test-Path "$destination/ollama.exe")) {
  throw "Ollama runtime archive did not contain ollama.exe"
}
