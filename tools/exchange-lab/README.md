# Laboratorio de Exchange (F8.5)

Una VM de VirtualBox con Windows Server 2022, Active Directory (`iuredesk.test`) y Exchange
(SE o 2019 CU15) para probar IureDesk contra un Exchange real: Autodiscover, Basic, NTLM,
Kerberos, correo, calendario, contactos, tareas y disponibilidad.

- VM `iuredesk-ex1`: 4 CPU, 12 GB (`RAM_MB=16384 vm.sh …` para más), disco de 150 GB dinámico.
- Red: NAT (internet para descargar requisitos) y solo-anfitrión `192.168.56.10`.
- Nombres: `ex1.iuredesk.test` (EWS), `autodiscover.iuredesk.test`, reino Kerberos `IUREDESK.TEST`.
- Carpeta de trabajo: `~/VirtualBox VMs/iuredesk-ex1/lab` (fuera del proyecto y de MEGA; `LAB_DIR=…`
  para otra). `vm.sh` copia ahí los scripts; la VM la ve como `Z:\`. Ahí quedan las contraseñas
  aleatorias (`lab.env`) y el certificado (`iuredesk-lab.crt`).
- Buzones: inician sesión como `ana@iuredesk.test` y `bea@iuredesk.test` (UPN) y su correo es
  `ana@iuredesk.internal` y `bea@iuredesk.internal`: Exchange no entrega correo a dominios reservados
  por la RFC 2606 como `.test` («501 5.1.4 Recipient address reserved by RFC 2606»; paso 5b).

Las versiones de evaluación funcionan 180 días sin clave.

## 1. Crear la VM (anfitrión)

Descarga las ISO (Microsoft pide un formulario): **Windows Server 2022 Evaluation** (inglés) y
**Exchange Server SE** (o 2019 CU15). Después:

```bash
tools/exchange-lab/vm.sh ~/Descargas/<windows-server-2022>.iso ~/Descargas/<exchange>.iso
```

Windows se instala solo (20–40 min, varios reinicios) y entra como `Administrator` con la
contraseña `ADMIN_PASSWORD` de `lab.env`. Si cambias un script en el proyecto, cópialo de nuevo a la
carpeta de trabajo.

## 2–5. Dentro de la VM

PowerShell **como administrador**; los scripts están en `Z:\` (carpeta compartida). Los scripts van en UTF-8 **con
marca (BOM)**: sin ella, Windows PowerShell 5 lee los acentos mal («PÃ©rez»). Si PowerShell
no deja ejecutarlos: `Set-ExecutionPolicy -Scope Process Bypass`.

| Paso | Comando | Entra como | Al terminar |
|---|---|---|---|
| 2 | `Z:\2-dominio.ps1` | `Administrator` | reinicia (ya es controlador de dominio) |
| 3 | `Z:\3-requisitos.ps1` | `IUREDESK\Administrator` | reinicia |
| 4 | `Z:\4-exchange.ps1` (30–60 min) | `IUREDESK\Administrator` | reinicia |
| 5 | `Z:\5-configurar.ps1` | `IUREDESK\Administrator` | deja `iuredesk-lab.crt` en la carpeta |
| 5b | `Z:\5b-correo.ps1` | `IUREDESK\Administrator` | correo en `@iuredesk.internal` (ver abajo) |

Si un paso falla, se puede volver a correr. Después del paso 5, una instantánea para volver
atrás cuando haga falta:

```bash
VBoxManage snapshot iuredesk-ex1 take exchange-listo
```

## 6. Anfitrión: nombres y certificado (una vez, con sudo)

```bash
echo "192.168.56.10 ex1.iuredesk.test autodiscover.iuredesk.test iuredesk.test" | sudo tee -a /etc/hosts
```

```bash
sudo cp ~/"VirtualBox VMs/iuredesk-ex1/lab/iuredesk-lab.crt" /usr/local/share/ca-certificates/ && sudo update-ca-certificates
```

IureDesk confía en las raíces del sistema además de las de Mozilla (lo mismo que necesita un
Exchange de empresa con CA interna), así que no hace falta nada más.

## Probar cada forma de entrar

En la VM, `Z:\6-modo.ps1 -Modo basic|ntlm|kerberos`:

- `basic`: Basic, NTLM y Negotiate (lo de siempre).
- `ntlm`: sin Basic; IureDesk debe pasar solo a NTLM.
- `kerberos`: sin Basic y solo Negotiate (como un dominio que restringe NTLM); IureDesk pide el
  ticket al KDC (`iuredesk.test:88`).

## Quitar el laboratorio

```bash
VBoxManage controlvm iuredesk-ex1 poweroff; VBoxManage unregistervm iuredesk-ex1 --delete
```

Y quitar la línea de `/etc/hosts` y `/usr/local/share/ca-certificates/iuredesk-lab.crt`
(`sudo update-ca-certificates --fresh`).
