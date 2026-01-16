# Скрипт для підготовки релізу SpeakV
# Цей скрипт збирає проект і перейменовує .exe файл для коректної роботи авто-оновлення

$PROJECT_ROOT = Get-Location
$TARGET_TRIPLE = "x86_64-pc-windows-msvc"
$BIN_NAME = "speakv"

Write-Host "🚀 Починаю збірку релізу..." -ForegroundColor Cyan

# 1. Запуск збірки через існуючий build.ps1
powershell -ExecutionPolicy Bypass -File .\build.ps1

if ($LASTEXITCODE -ne 0) {
    Write-Host "❌ Помилка збірки!" -ForegroundColor Red
    exit $LASTEXITCODE
}

# 2. Шляхи
$BUILD_EXE = "$PROJECT_ROOT\target\release\$BIN_NAME.exe"
$RELEASE_DIR = "$PROJECT_ROOT\dist"
$RELEASE_EXE = "$RELEASE_DIR\$BIN_NAME-$TARGET_TRIPLE.exe"

# 3. Створення папки dist
if (!(Test-Path $RELEASE_DIR)) {
    New-Item -ItemType Directory -Path $RELEASE_DIR
}

# 4. Копіювання та перейменування
Write-Host "📦 Підготовка файлу для GitHub..." -ForegroundColor Yellow
Copy-Item $BUILD_EXE $RELEASE_EXE -Force

Write-Host "✅ Готово!" -ForegroundColor Green
Write-Host "-------------------------------------------"
Write-Host "Тепер завантажте цей файл у реліз на GitHub:" -ForegroundColor White
Write-Host "$RELEASE_EXE" -ForegroundColor Cyan
Write-Host "-------------------------------------------"
