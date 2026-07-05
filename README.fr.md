# motu-pci-424

*Lire dans une autre langue : [English](README.md) · **Français***

🌐 **Site web :** <https://devfrp.github.io/linux-motu-pci-424/>

Un pilote ALSA Linux écrit de zéro pour la carte audio **MOTU PCI-324 / PCI-424**
et ses interfaces de sortie AudioWire (2408, 24I/O, 828, HD192, 896HD, …).

> **État : ossature complète, protocole matériel non confirmé.**
> Toute la mécanique PCI / DMA / IRQ / ALSA est réelle et complète. La MOTU
> PCI-324/424 n'est pas documentée, donc la *carte des registres et le protocole
> DMA/transport* sont une hypothèse documentée à valider sur une vraie carte.
> D'ici là, le module se charge et crée un périphérique ALSA mais ne produira pas
> un son correct. Voir [Rétro-ingénierie](#rétro-ingénierie).

## Démarrage rapide

```sh
curl -fsSL https://raw.githubusercontent.com/devfrp/linux-motu-pci-424/main/get.sh | sh
```

Installe sur n'importe quelle distro (dépendances + module DKMS + outils).
Détails et options dans [Installation](#installation-toute-distro).

## Organisation

| Chemin | Rôle |
|--------|------|
| `kernel/motu424.h` | Définitions partagées + la **carte des registres hypothétique** (source de vérité unique) |
| `kernel/motu424_main.c` | Attach/detach PCI, gestion des ressources + IRQ, gestionnaire d'interruption |
| `kernel/motu424_hw.c` | **Abstraction matérielle — le seul fichier avec la vraie sémantique des registres** |
| `kernel/motu424_pcm.c` | Callbacks PCM ALSA (lecture + capture) |
| `tools/motu424-probe.c` | Dumpeur de BAR0 en espace utilisateur pour la rétro-ingénierie |
| `tools/motu424-ctl.c` | **CLI de gestion façon CueMix** (horloge/format + mixeur de monitoring) via alsa-lib |
| `tools/motu424-gui` | **GUI GTK4** (panneau de contrôle, front-end de `motu424-ctl`) |
| `tools/re/` | Aides à la RE statique (`vtable-scan.py`, `xref.py` basé sur capstone) |
| `get.sh` | **Bootstrap `curl \| sh`** — récupère les sources + lance l'installeur |
| `install.sh` | **Installeur multi-distro** (dépendances + DKMS + outils) |
| `ARCHITECTURE.md` | Notes de conception : la séparation en 3 couches + la règle de confinement matériel |
| `dkms.conf` | Empaquetage DKMS pour la reconstruction automatique à chaque noyau |

Objectif de conception : **toute l'incertitude est confinée à `motu424.h` +
`motu424_hw.c`.** Une fois la vraie disposition des registres connue, seuls ces
deux fichiers changent.

## Installation (toute distro)

Une ligne — récupère les sources et lance l'installeur :

```sh
curl -fsSL https://raw.githubusercontent.com/devfrp/linux-motu-pci-424/main/get.sh | sh
# passer des options à l'installeur :
curl -fsSL https://raw.githubusercontent.com/devfrp/linux-motu-pci-424/main/get.sh | sh -s -- --no-dkms -y
```

Ou clonez et lancez l'installeur directement. Il détecte votre gestionnaire de
paquets (pacman/apt/dnf/yum/zypper/apk/xbps), tire les dépendances de build,
installe le module via **DKMS** (pour qu'il survive aux mises à jour du noyau) et
installe les outils :

```sh
./install.sh              # deps + DKMS + outils + chargement (utilise sudo au besoin)
./install.sh -y           # installation de paquets non interactive
./install.sh --no-dkms    # build in-tree + install au lieu de DKMS
./install.sh --uninstall  # retire le module + les outils
./install.sh -h           # toutes les options
```

Sous `curl | sh`, l'installeur détecte l'absence de terminal et bascule
automatiquement les installations de paquets en mode non interactif ; sudo
demande son mot de passe via le terminal comme d'habitude.

## Build (manuel)

Nécessite les en-têtes du noyau en cours (Arch : `linux-rt-headers` pour un
noyau RT, sinon `linux-headers`) et `alsa-lib` pour `motu424-ctl`.

```sh
make            # build module + outils
make load       # insmod kernel/motu424.ko
dmesg | tail    # chercher « MOTU PCI-4xx registered as ALSA card N »
aplay -l        # la carte devrait apparaître
make unload
```

### Gérer la carte (CueMix pour Linux)

`tools/motu424-ctl` est l'appli de contrôle en espace utilisateur — l'équivalent
Linux du CueMix FX de MOTU. Elle localise automatiquement la carte MOTU et gère
les kcontrols du mixeur ALSA du pilote (source d'horloge, fréquence
d'échantillonnage, trim/pad/phase par entrée, et la matrice de monitoring
entrées×bus). Jeu de contrôles + nommage : [`docs/cuemix-control-map.md`](docs/cuemix-control-map.md).

```sh
make tools                              # compile motu424-ctl si alsa-lib est présent
./tools/motu424-ctl                     # aperçu d'état façon CueMix
./tools/motu424-ctl list                # tous les contrôles
./tools/motu424-ctl set 'Clock Source' Internal
./tools/motu424-ctl set 'Mix 00 Input 03 Volume' 92
```

Les contrôles du mixeur relèvent de la Phase 5 (dépendante de la carte) ; l'appli
est écrite pour s'activer automatiquement dès que le pilote les enregistre, et se
dégrade proprement en leur absence.

#### Panneau graphique

`tools/motu424-gui` est un panneau de contrôle GTK4 — un simple front-end
au-dessus de `motu424-ctl`, de sorte que la CLI reste la source de vérité unique.
Il choisit la carte, puis rend chaque contrôle selon son type (interrupteur /
curseur / menu déroulant), regroupé façon CueMix (horloge & format, entrées,
matrice de mix par bus).

```sh
./install.sh --gui        # installe le lanceur + ses dépendances d'exécution
motu424-gui               # ou depuis le menu d'applications (« MOTU CueMix »)
```

Nécessite `python-gobject` + `gtk4` (ajoutés automatiquement par `--gui`). Comme
la CLI, il s'ouvre sur n'importe quelle carte ALSA et affiche la disposition
CueMix complète dès que le pilote expose ses kcontrols.

## Rétro-ingénierie

La carte n'est pas présente sur la machine de développement, donc les offsets de
registres dans `kernel/motu424.h` (marqués `TODO: verify`) sont des hypothèses.
Pour les confirmer :

1. **Identifier la carte.** `lspci -nn | grep -i 137a` (0x137A = Mark of the Unicorn).
   Mettre à jour `PCI_DEVICE_ID_MOTU_PCI*` si l'ID de périphérique rapporté diffère.
2. **Dumper BAR0** avec le pilote constructeur *non lié* :
   ```sh
   sudo ./tools/motu424-probe
   ```
3. **Differ** un dump pris au repos contre un pris pendant que la carte diffuse de
   l'audio sous l'OS constructeur (macOS/Windows) pour localiser les registres
   d'état, de position DMA et d'IRQ.
4. **Tracer l'anneau DMA / le format d'événement** (nombre de canaux par famille
   de fréquence, empaquetage 24 bits, boutisme).
5. Mettre à jour les constantes dans `motu424.h` et les encodeurs dans `motu424_hw.c`.

Une grande partie du protocole a **déjà** été récupérée statiquement depuis le
pilote constructeur (sans carte) — les faits confirmés ci-dessous remplacent les
suppositions de l'en-tête ; les lacunes restantes sont ce que les étapes 1–5
referment sur du vrai matériel.

### Découvertes issues de la RE du pilote constructeur (statique, sans carte)

Établies en désassemblant le pilote Windows constructeur `MOTUAW.sys`, avec
`objdump`, `tools/re/vtable-scan.py` (résout les slots de vtable C++) et
`tools/re/xref.py` (références croisées basées sur capstone — appelants, corps de
fonction, sites de dispatch virtuel). Elles **remplacent la carte des registres
hypothétique** de `motu424.h` :

- **Deux fenêtres MMIO, pas un petit banc de registres.** La carte expose un
  espace d'« adresse carte » fenêtré d'environ 24 bits. Une adresse 32 bits est
  routée par `(addr & 0xff800000) == 0x01800000` : si vrai → fenêtre A
  `(addr & 0x7fffff)` (8 Mo), sinon → fenêtre B `(addr & 0x3fffff)` (4 Mo).
  Registres ctrl/état par banc à `0xC0024` / `0x100024` (banc+`0x24`, pas
  `0x40000`), atteignables via la fenêtre A à `0x18C0000` / `0x1900000`. (`0xAC44`
  n'est **pas** une adresse — c'est 44100 en décimal ; les six fréquences
  apparaissent comme constantes Hz `0xAC44`..`0x2EE00` dans les calculs de diviseur.)
- **Un petit BAR de ports d'E/S** (`READ/WRITE_PORT_ULONG`) porte le contrôle
  bridge/GPIO : lire `+0x0` (bit 1 = IRQ en attente), écrire `+0x0`←4 (activation
  IRQ/flux), écrire `+0x4`←1 (strobe/commit). Un pont de bus local façon PLX
  devant le FPGA.
- **Chemin IRQ — confirmé** (ISR = vtable `0x30cc0` slot `0x28` = `0x2bae0`) :
  en attente = BAR de ports `+0x0` bit 1 ; **ack = écrire `0x10`** à l'adresse
  carte en device-ext `+0x88` ; « période écoulée » se déclenche quand un
  accumulateur par IRQ franchit `0x800`, puis une DPC est mise en file.
- **Bloc de registres audio — confirmé** (base = une adresse carte en device-ext
  `+0x98`) : `base+0x54`←1 activation du flux, `base+0x60` incrément de période
  (`0x10 << 2*family`), `base+0x64` paramètre fréquence/horloge, `base+0x128/12c/130`
  compteurs de position (lus puis remis à zéro à chaque période).
- **Le transport audio est du PIO vers une ouverture de la carte, pas du DMA
  bus-master hôte.** L'assistant de push (`fn 0x29420`) copie les tampons hôte
  dans la fenêtre B via `WRITE_REGISTER_BUFFER_ULONG` (un tampon rebond de 64 Ko
  marqué `'MOTU'`) et suit un `dmaPoint` matériel + `readHead`/`writeHead`/`len`
  logiciels.
- **Firmware FPGA.** Deux architectures : la PCI-324/424 classique utilise un FPGA
  Altera en série passive (`altera424b.rbf`, **absent** de l'installeur constructeur
  4.0.6 — probablement auto-configuré depuis une flash embarquée) ; la PCIe
  **HD Express** utilise un SoC ARM + Xilinx Virtex, livré sous forme de
  `HDExpress_FullImageRun.bin` (un conteneur à en-tête de 24 octets, checksum
  vérifié). `MOTUAW.sys` n'a aucune E/S fichier, donc un pilote Linux fournira le
  firmware via `request_firmware()` là où c'est nécessaire.
- **Jeu de contrôles CueMix** décodé depuis le `CueMixFX-PCI-424.touchosc` fourni
  (l'API OSC CueMix de MOTU) : une matrice de monitoring entrées×bus + le
  conditionnement analogique par entrée + horloge/format. Pilote
  `tools/motu424-ctl` — voir [`docs/cuemix-control-map.md`](docs/cuemix-control-map.md).

Les rédactions détaillées, étiquetées par niveau de preuve, vivent dans `docs/` :
[`register-map.md`](docs/register-map.md), [`transport.md`](docs/transport.md),
[`fpga-upload.md`](docs/fpga-upload.md),
[`cuemix-control-map.md`](docs/cuemix-control-map.md),
[`vendor-driver-map.md`](docs/vendor-driver-map.md).

Encore ouvert (nécessite la carte, une récupération de types niveau Ghidra, ou une
RE plus poussée) : le registre/bits de sélection de source d'horloge et l'encodage
du paramètre `base+0x64`, les valeurs numériques à l'exécution de la base audio /
adresse d'ack, et le handshake d'upload du FPGA.

La feuille de route complète et par phases, d'ici à un pilote 100 % fonctionnel,
vit dans [`docs/REVERSE_ENGINEERING_PLAN.md`](docs/REVERSE_ENGINEERING_PLAN.md).

## Licence

GPL-2.0-or-later. Les modules noyau liant des symboles ALSA GPL-only doivent être GPL.
