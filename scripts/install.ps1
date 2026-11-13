# Downloads and verifies the latest zerd release into %LOCALAPPDATA%\zerd\bin and adds it to your user PATH.
#   irm https://zerd.si/install.ps1 | iex
$ErrorActionPreference = 'Stop'
if (-not [Environment]::Is64BitOperatingSystem) { throw 'zerd needs 64-bit Windows' }
if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { Write-Host 'ARM64 Windows: running the x64 build under emulation' }

$dir = if ($env:ZERD_BIN) { $env:ZERD_BIN } else { Join-Path $env:LOCALAPPDATA 'zerd\bin' }
New-Item -ItemType Directory -Force $dir | Out-Null
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("zerd-" + [Guid]::NewGuid())
New-Item -ItemType Directory $tmp | Out-Null
try {
  $base = 'https://github.com/Adityaadpandey/zerd.si/releases/latest/download/zerd-x86_64-pc-windows-msvc.zip'
  $zip = Join-Path $tmp 'zerd.zip'
  Invoke-WebRequest "$base" -OutFile $zip
  $want = ((Invoke-WebRequest "$base.sha256").Content.Trim() -split '\s+')[0]
  $got = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
  if ($want -ne $got) { throw 'checksum mismatch, not installing' }
  Expand-Archive $zip -DestinationPath $dir -Force
} finally { Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue }

$user = [Environment]::GetEnvironmentVariable('Path', 'User')
if (($user -split ';') -notcontains $dir) {
  [Environment]::SetEnvironmentVariable('Path', "$user;$dir", 'User')
  Write-Host "added $dir to your PATH (open a new terminal)"
}
Write-Host "installed $dir\zerd.exe"
if (-not (Get-Command git -ErrorAction SilentlyContinue)) { Write-Host 'note: zerd needs Git for Windows (https://git-scm.com/download/win); it also provides the bash zerd runs commands in' }
Write-Host ''
Write-Host 'Next: open a new terminal, cd into a repo and run  zerd  (first launch walks you through connecting a model; zerd doctor checks the setup)'
