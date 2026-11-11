# Downloads and verifies the latest melon release into %LOCALAPPDATA%\melon\bin and adds it to your user PATH.
#   irm https://raw.githubusercontent.com/Adityaadpandey/melon-harness/main/install.ps1 | iex
$ErrorActionPreference = 'Stop'
if (-not [Environment]::Is64BitOperatingSystem) { throw 'melon needs 64-bit Windows' }
if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { Write-Host 'ARM64 Windows: running the x64 build under emulation' }

$dir = if ($env:MELON_BIN) { $env:MELON_BIN } else { Join-Path $env:LOCALAPPDATA 'melon\bin' }
New-Item -ItemType Directory -Force $dir | Out-Null
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("melon-" + [Guid]::NewGuid())
New-Item -ItemType Directory $tmp | Out-Null
try {
  $base = 'https://github.com/Adityaadpandey/melon-harness/releases/latest/download/melon-x86_64-pc-windows-msvc.zip'
  $zip = Join-Path $tmp 'melon.zip'
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
Write-Host "installed $dir\melon.exe"
if (-not (Get-Command git -ErrorAction SilentlyContinue)) { Write-Host 'note: melon needs Git for Windows (https://git-scm.com/download/win); it also provides the bash melon runs commands in' }
