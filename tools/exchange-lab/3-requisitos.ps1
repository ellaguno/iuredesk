# Paso 3 (VM, entrando como IUREDESK\Administrator): requisitos de Exchange. Reinicia al terminar.
. Z:\comun.ps1
try { Add-DnsServerForwarder -IPAddress 10.0.2.3 -ErrorAction Stop } catch { }
Install-WindowsFeature Server-Media-Foundation, NET-Framework-45-Features, RPC-over-HTTP-proxy, RSAT-Clustering, `
    RSAT-Clustering-CmdInterface, RSAT-Clustering-Mgmt, RSAT-Clustering-PowerShell, WAS-Process-Model, Web-Asp-Net45, `
    Web-Basic-Auth, Web-Client-Auth, Web-Digest-Auth, Web-Dir-Browsing, Web-Dyn-Compression, Web-Http-Errors, `
    Web-Http-Logging, Web-Http-Redirect, Web-Http-Tracing, Web-ISAPI-Ext, Web-ISAPI-Filter, Web-Lgcy-Mgmt-Console, `
    Web-Metabase, Web-Mgmt-Console, Web-Mgmt-Service, Web-Net-Ext45, Web-Request-Monitor, Web-Server, `
    Web-Stat-Compression, Web-Static-Content, Web-Windows-Auth, Web-WMI, Windows-Identity-Foundation, RSAT-ADDS | Out-Null
$dl = 'C:\lab-descargas'
New-Item -ItemType Directory -Force $dl | Out-Null
$items = @(
    @{ Name = '.NET Framework 4.8.1'; Url = 'https://go.microsoft.com/fwlink/?linkid=2203305'; File = 'ndp481.exe'; Args = '/q /norestart' },
    @{ Name = 'Visual C++ 2012 x64'; Url = 'https://download.microsoft.com/download/1/6/B/16B06F60-3B20-4FF2-B699-5E9B7962F9AE/VSU_4/vcredist_x64.exe'; File = 'vcredist2012_x64.exe'; Args = '/install /quiet /norestart' },
    @{ Name = 'Visual C++ 2013 x64'; Url = 'https://aka.ms/highdpimfc2013x64enu'; File = 'vcredist2013_x64.exe'; Args = '/install /quiet /norestart' },
    @{ Name = 'IIS URL Rewrite 2.1'; Url = 'https://download.microsoft.com/download/1/2/8/128E2E22-C1B9-44A4-BE2A-5859ED1D4592/rewrite_amd64_en-US.msi'; File = 'rewrite_amd64.msi'; Args = $null }
)
foreach ($i in $items) {
    $f = Join-Path $dl $i.File
    if (-not (Test-Path $f)) {
        Write-Host "Descargando $($i.Name)…"
        try { Invoke-WebRequest -Uri $i.Url -OutFile $f -UseBasicParsing }
        catch { throw "No se pudo descargar $($i.Name) ($($i.Url)). Descárgalo a mano en $f y vuelve a correr este paso." }
    }
    Write-Host "Instalando $($i.Name)…"
    if ($i.File -like '*.msi') { Start-Process msiexec.exe -ArgumentList "/i `"$f`" /qn /norestart" -Wait }
    else { Start-Process $f -ArgumentList $i.Args -Wait }
}
$ex = ExchangeDrive
Write-Host 'Instalando UCMA 4.0 (de la ISO de Exchange)…'
Start-Process "$($ex)UCMARedist\Setup.exe" -ArgumentList '/passive /norestart' -Wait
Write-Host 'Requisitos listos; reiniciando.'
Restart-Computer -Force
