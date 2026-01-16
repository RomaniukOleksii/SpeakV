# Find VS Installation
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$vsPath = & $vswhere -latest -products * -property installationPath

if (-not $vsPath) {
    Write-Error "Visual Studio installation not found."
    exit 1
}

# Find vcvars64.bat
$vcvars = Get-ChildItem -Path "$vsPath\VC\Auxiliary\Build" -Filter "vcvars64.bat" | Select-Object -First 1

if (-not $vcvars) {
    Write-Error "vcvars64.bat not found."
    exit 1
}

# Use cmd to run vcvars and export environment
$tempFile = [System.IO.Path]::GetTempFileName()
cmd /c " `"$($vcvars.FullName)`" && set > `"$tempFile`" "

# Import variables into child PowerShell session
Get-Content $tempFile | ForEach-Object {
    if ($_ -match '^(.*?)=(.*)$') {
        $name = $matches[1]
        $value = $matches[2]
        
        # Set for the current process
        [Environment]::SetEnvironmentVariable($name, $value, [EnvironmentVariableTarget]::Process)
        
        # Also update the $env: provider for convenience
        switch ($name) {
            "PATH" { $env:PATH = $value }
            "INCLUDE" { $env:INCLUDE = $value }
            "LIB" { $env:LIB = $value }
            "LIBPATH" { $env:LIBPATH = $value }
        }
    }
}
Remove-Item $tempFile -Force

# Run Cargo Build
Write-Host "MSVC Environment initialized via vcvars64.bat. Running cargo build..." -ForegroundColor Cyan
cargo build --release
