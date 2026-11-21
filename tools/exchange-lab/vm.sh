#!/usr/bin/env bash
# Crea la VM de laboratorio de Exchange (F8.5) en VirtualBox con instalación desatendida de
# Windows Server 2022. Uso:
#   tools/exchange-lab/vm.sh <ISO de Windows Server 2022> <ISO de Exchange>
# Copia los scripts a una carpeta de trabajo fuera del proyecto (y fuera de MEGA: ahí van las
# contraseñas y el certificado), por omisión ~/VirtualBox VMs/iuredesk-ex1/lab, que la VM ve como Z:.
# Genera (una vez) las contraseñas de laboratorio en <carpeta>/lab.env.
set -euo pipefail

VM=iuredesk-ex1
HERE="$(cd "$(dirname "$0")" && pwd)"
BASE="$HOME/VirtualBox VMs"
LAB="${LAB_DIR:-$BASE/$VM/lab}"
WIN_ISO="${1:?falta la ISO de Windows Server 2022}"
EX_ISO="${2:?falta la ISO de Exchange}"
RAM_MB="${RAM_MB:-12288}"
CPUS="${CPUS:-4}"
DISK_MB="${DISK_MB:-153600}"

[ -f "$WIN_ISO" ] || { echo "no existe $WIN_ISO" >&2; exit 1; }
[ -f "$EX_ISO" ] || { echo "no existe $EX_ISO" >&2; exit 1; }
if VBoxManage showvminfo "$VM" >/dev/null 2>&1; then
  echo "la VM $VM ya existe (bórrala con: VBoxManage unregistervm $VM --delete)" >&2
  exit 1
fi

# Carpeta de trabajo con los scripts (se actualizan si cambian en el proyecto).
mkdir -p "$LAB"
cp "$HERE"/*.ps1 "$HERE/README.md" "$LAB/"
echo "carpeta del laboratorio: $LAB"

# Contraseñas de laboratorio: aleatorias, solo en este equipo. Sin el nombre del usuario: la
# política de complejidad de Active Directory rechaza una contraseña que lo contenga.
ENV="$LAB/lab.env"
if [ ! -f "$ENV" ]; then
  gen() { tr -dc 'A-Za-z0-9' </dev/urandom | head -c 14; }
  umask 077
  cat >"$ENV" <<VARS
# Laboratorio de Exchange de IureDesk (F8.5). Solo para la VM de pruebas; no subir a git.
ADMIN_PASSWORD=Lab-$(gen)-1a
ANA_PASSWORD=Lab-$(gen)-7x
BEA_PASSWORD=Lab-$(gen)-8y
VARS
  echo "contraseñas de laboratorio en $ENV"
fi
# shellcheck disable=SC1090
. "$ENV"

# Red solo-anfitrión 192.168.56.0/24 (la VM será 192.168.56.10).
IF="$(VBoxManage list hostonlyifs | awk '/^Name:/{n=$2} /^IPAddress: *192\.168\.56\.1$/{print n; exit}')"
if [ -z "$IF" ]; then
  VBoxManage hostonlyif create
  IF="$(VBoxManage list hostonlyifs | awk '/^Name:/{n=$2} END{print n}')"
  VBoxManage hostonlyif ipconfig "$IF" --ip 192.168.56.1 --netmask 255.255.255.0
fi
echo "red solo-anfitrión: $IF"

VBoxManage createvm --name "$VM" --ostype Windows2022_64 --basefolder "$BASE" --register
VBoxManage modifyvm "$VM" --memory "$RAM_MB" --cpus "$CPUS" --firmware efi --vram 64 \
  --graphicscontroller vboxsvga --ioapic on --rtcuseutc on \
  --nic1 nat --nic2 hostonly --hostonlyadapter2 "$IF" --clipboard-mode bidirectional
VBoxManage createmedium disk --filename "$BASE/$VM/$VM.vdi" --size "$DISK_MB" --format VDI --variant Standard
# Caché de E/S del anfitrión: si el anfitrión está ocupado (p. ej. compilando), sin ella las
# escrituras tardan tanto que Windows da el disco por muerto («AHCI#0: Port 0 reset») y la
# instalación se queda congelada.
VBoxManage storagectl "$VM" --name SATA --add sata --controller IntelAhci --portcount 4 --hostiocache on
VBoxManage storageattach "$VM" --storagectl SATA --port 0 --device 0 --type hdd --medium "$BASE/$VM/$VM.vdi"
# Carpeta compartida con los scripts (\\VBOXSVR\lab, montada en Z:).
VBoxManage sharedfolder add "$VM" --name lab --hostpath "$LAB" --automount --auto-mount-point 'Z:'

# Las ediciones de evaluación no llevan clave, y un <ProductKey> vacío (lo pone la plantilla de
# VirtualBox aunque no haya clave) hace fallar a Setup con «cannot find the Microsoft Software
# License Terms»: se usa una copia de la plantilla sin ese bloque.
TEMPLATE="$LAB/autounattend-sin-clave.xml"
sed '/<ProductKey>/,/<\/ProductKey>/d' /usr/share/virtualbox/UnattendedTemplates/win_nt6_unattended.xml >"$TEMPLATE"
grep -q ProductKey "$TEMPLATE" && { echo "no se pudo quitar <ProductKey> de la plantilla" >&2; exit 1; }

# Windows Server 2022 Standard (escritorio), desatendido, con Guest Additions.
VBoxManage unattended install "$VM" --iso="$WIN_ISO" --user=Administrator --password="$ADMIN_PASSWORD" \
  --script-template="$TEMPLATE" \
  --full-user-name="Laboratorio IureDesk" --hostname=ex1.iuredesk.test --install-additions \
  --image-index=2 --locale=en_US --country=MX --time-zone="Central Standard Time (Mexico)" --post-install-command="shutdown /r /t 5"
# La ISO de Exchange en la segunda unidad (la de Windows la pone «unattended»).
VBoxManage storageattach "$VM" --storagectl SATA --port 2 --device 0 --type dvddrive --medium "$EX_ISO"

VBoxManage startvm "$VM" --type gui
cat <<MSG

La VM $VM se está instalando sola (20–40 min, varios reinicios). Cuando veas el escritorio,
sigue tools/exchange-lab/README.md desde el paso 2 (dentro de la VM, PowerShell como
administrador, scripts en Z:\\). Contraseñas y certificado: $LAB
MSG
