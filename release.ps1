$TARGET_TRIPLE = "x86_64-pc-windows-msvc"
$BIN_NAME = "speakv"

Write-Host "Starting release build..."

& powershell -ExecutionPolicy Bypass -File .\build.ps1

if ($LASTEXITCODE -ne 0) {
    Write-Host "Build failed!"
    exit $LASTEXITCODE
}

$BUILD_EXE = Join-Path $PSScriptRoot "target\release\$BIN_NAME.exe"
$RELEASE_DIR = Join-Path $PSScriptRoot "dist"
$RELEASE_EXE = Join-Path $RELEASE_DIR "$BIN_NAME-$TARGET_TRIPLE.exe"

if (!(Test-Path $RELEASE_DIR)) {
    New-Item -ItemType Directory -Path $RELEASE_DIR | Out-Null
}

if (Test-Path $BUILD_EXE) {
    Copy-Item -Path $BUILD_EXE -Destination $RELEASE_EXE -Force
    Write-Host "Release file ready at: $RELEASE_EXE"
}
else {
    Write-Host "Error: $BUILD_EXE not found!"
    exit 1
}
