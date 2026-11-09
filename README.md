# Euhpen

Application open source qui coupe les **silences** et les **hésitations** (« euh », « hum »,
syllabes étirées) dans des rushes où une seule personne parle. Elle exporte une **timeline XML
pour Premiere Pro et DaVinci Resolve**. Les fichiers originaux ne sont jamais modifiés : la
timeline y fait référence telle quelle, et vous pouvez étirer les bords des clips dans votre
logiciel pour redonner de l'air.

Tout se fait sur votre ordinateur, hors ligne : Windows, macOS (Apple Silicon) et Linux.

- Application native écrite en Rust (aucune page web, aucun serveur local), avec :
  - un aperçu qui saute déjà les coupes ;
  - une forme d'onde colorée par motif de coupe ;
  - des curseurs qui recalculent en direct ;
  - des corrections au clic ;
  - une transcription cliquable.
- Détection des hésitations entraînée sur du français : Whisper et une petite tête de
  classification, dans un processus Python séparé. Voir [Hésitations](#hésitations).
- Ligne de commande pour les lots et l'automatisation.
- Presets, statistiques par fichier et totales, traitement de dossiers entiers.

> **Version 0.1.0, première version publique.** Elle a été validée sous Windows avec une carte
> NVIDIA, et sur un Mac à puce Apple pour l'application et le calcul sur MPS. Linux n'a pas encore
> été essayé : voir [État et limites](#état-et-limites).

## Installation

### Windows : l'installateur

Téléchargez `Euhpen-Setup-0.1.0.exe` depuis la
[dernière version](https://github.com/ArthurArctique/Euhpen/releases/latest), puis ouvrez-le.
Choisissez le dossier, cochez ce que vous voulez et cliquez sur **Installer**. Il ne faut ni droits
d'administrateur, ni Rust, ni Python.

| Composant | Rôle | Espace |
| --- | --- | --- |
| Euhpen | l'application et la ligne de commande | 40 Mo |
| ffmpeg | lecture des médias ; téléchargé s'il n'est pas déjà présent | 210 Mo |
| Python 3.12 et PyTorch | détection des « euh », facultative | 4,4 Go avec carte NVIDIA (CUDA 12.8), 0,7 Go sinon |
| Whisper | facultatif ici : sinon téléchargé à la première détection | 1,6 Go |

Tout va dans `%LOCALAPPDATA%\Programs\Euhpen`. Seul Whisper est rangé à part, dans le cache
Hugging Face. À la fin, l'installateur vérifie que PyTorch voit réellement la carte NVIDIA et
affiche le résultat. Après une coupure, **Réessayer** reprend là où l'installation s'est arrêtée.

L'installateur n'étant pas signé, Windows peut afficher un avertissement SmartScreen : cliquez
sur **Informations complémentaires**, puis **Exécuter quand même**.

Pour **mettre à jour**, ouvrez le nouvel installateur : il remplace l'application et garde vos
réglages, vos presets et le modèle. Pour **désinstaller** : Paramètres de Windows > Applications >
Euhpen. Vos rushes et vos timelines ne sont jamais touchés.

### macOS (puce Apple) : l'installateur

Téléchargez `Euhpen-Setup-0.1.0-macos-arm64.dmg` depuis la
[dernière version](https://github.com/ArthurArctique/Euhpen/releases/latest), ouvrez-le, puis
ouvrez **Installer Euhpen**. Il faut macOS 12 ou plus récent et un Mac M1 ou plus récent. Il ne
faut ni mot de passe administrateur, ni Rust, ni Python.

| Composant | Rôle | Espace |
| --- | --- | --- |
| Euhpen | l'application et la ligne de commande | 30 Mo |
| ffmpeg | lecture des médias ; téléchargé s'il n'est pas déjà présent | 130 Mo |
| Python 3.12 et PyTorch | détection des « euh », facultative ; calcul sur la puce graphique (MPS) | environ 1 Go |
| Whisper | facultatif ici : sinon téléchargé à la première détection | 1,6 Go |

Tout va dans `~/Applications/Euhpen`, sauf Whisper, rangé dans le cache Hugging Face.

L'installateur n'étant pas signé par un compte développeur Apple, macOS refuse de l'ouvrir la
première fois. Fermez le message, puis allez dans **Réglages Système > Confidentialité et
sécurité** et cliquez sur **Ouvrir quand même**. Le fichier `LISEZ-MOI.txt` de l'image disque
donne aussi la commande équivalente pour le Terminal.

Pour **mettre à jour**, ouvrez le nouvel installateur : il remplace l'application et garde vos
réglages, vos presets et le modèle. Pour **désinstaller**, ouvrez **Désinstaller Euhpen** dans le
dossier d'installation. Vos rushes et vos timelines ne sont jamais touchés.

### Depuis le code source : double-clic

Récupérez le code : archive « Source code » de la
[dernière version](https://github.com/ArthurArctique/Euhpen/releases/latest), à décompresser, ou
`git clone https://github.com/ArthurArctique/Euhpen.git`. Puis :

- **Windows** : double-cliquez sur `Lancer Euhpen.bat`.
- **macOS** : double-cliquez sur `Lancer Euhpen.command` (si macOS refuse de l'ouvrir : clic
  droit > Ouvrir).

Sous Linux, suivez l'[installation à la main](#à-la-main).

Le script installe ce qui manque, en demandant confirmation, puis compile Euhpen et ouvre
l'application. La première fois prend quelques minutes ; ensuite, l'ouverture est immédiate.

| Élément | Windows | macOS |
| --- | --- | --- |
| ffmpeg | winget | Homebrew |
| Rust | winget (rustup), sans Visual Studio ni MinGW | Homebrew |
| Python 3.10+, pour les hésitations | déjà présent, ou `winget install Python.Python.3.12` | déjà présent, ou `brew install python@3.12` |

Le script demande aussi s'il faut installer la détection des hésitations (0,7 Go, ou 4,4 Go avec
PyTorch pour CUDA), dans un dossier `.venv` à côté. Whisper (1,6 Go) est téléchargé à la première
analyse. Pour revenir sur votre réponse, supprimez le fichier `.euhpen-install` et relancez.

**Carte NVIDIA** : le script installe PyTorch pour CUDA 12.8 (cartes RTX 20 à RTX 50), puis
vérifie que PyTorch voit réellement la carte. Sinon, il réinstalle la version CUDA, et vous
prévient si le problème persiste. La fenêtre **Système** de l'application indique le matériel
détecté et, après une transcription, celui qui a vraiment servi et la mémoire graphique utilisée.

### Mettre à jour depuis le code source

Avec git, lancez `git pull`, puis le lanceur : il recompile Euhpen et ne réinstalle le Python du
modèle que si `python/requirements.txt` a changé.

Avec une archive, décompressez la nouvelle version et utilisez son lanceur : le Python du modèle
est réinstallé dans ce nouveau dossier. Vos réglages, vos presets et le cache sont rangés hors du
dossier d'Euhpen : ils sont conservés.

### À la main

| Système | ffmpeg |
| --- | --- |
| macOS | `brew install ffmpeg` |
| Windows | `winget install Gyan.FFmpeg` |
| Linux | `sudo apt install ffmpeg` |

Il faut aussi [Rust](https://rustup.rs). Sous Linux, ajoutez `pkg-config` et `libasound2-dev`.

```bash
cargo build --release
```

Sous Windows sans Visual Studio, utilisez la même chaîne que le lanceur :

```bash
rustup toolchain install stable-x86_64-pc-windows-gnullvm --profile minimal
cargo +stable-x86_64-pc-windows-gnullvm build --release
```

Les exécutables sont dans `target/release/` : `euhpen-gui` (l'application) et `euhpen` (la ligne
de commande).

Pour les hésitations, créez un environnement Python à la racine du projet :

```bash
python -m venv .venv
.venv/bin/pip install -r python/requirements.txt
```

Sous Windows, le dossier est `.venv\Scripts\`.

- **Cartes NVIDIA sous Windows ou Linux** : installez d'abord PyTorch pour CUDA avec
  `pip install torch --index-url https://download.pytorch.org/whl/cu128`, sinon PyTorch
  tourne sur le processeur.
- **ffmpeg hors du PATH** : indiquez son chemin avec les variables `EUHPEN_FFMPEG` et
  `EUHPEN_FFPROBE`.
- **Autre emplacement du Python du modèle** : variable `EUHPEN_PYTHON`.
- **Dossier synchronisé par iCloud ou OneDrive** : `.venv` et `target` pèsent plusieurs Go.
  Mieux vaut placer le projet hors du dossier synchronisé.

## L'interface

Ouvrez-la avec le raccourci de l'installateur, le lanceur, `euhpen-gui`, ou `euhpen ui`. L'option `--fresh` ne recharge pas la
liste de fichiers précédente.

### 1. Ajouter des rushes

Utilisez « + Fichiers » ou « + Dossier » (fenêtre du système), collez un chemin, ou glissez
les fichiers sur la fenêtre.

- Chaque fichier est analysé une seule fois : le résultat est mis en cache.
- Un aperçu léger (H.264 540p ou 720p) est préparé pour la lecture.
- La liste est retrouvée au prochain lancement.

### 2. Régler

Les curseurs recalculent les coupes à chaque mouvement, sans réanalyser.

- **Seuil** : automatique par défaut, calculé entre le bruit de fond et la parole de chaque
  fichier. En manuel, la ligne pointillée de la forme d'onde se déplace à la souris.
- **Silence minimal**, **air avant/après la parole** : réglages principaux. Le reste est dans
  « Réglages avancés ».
- **Hésitations** : voir [plus bas](#hésitations).

### 3. Vérifier

Le lecteur propose trois modes :

| Mode | Lecture |
| --- | --- |
| **Après** | La vidéo telle qu'elle sera montée : les coupes sont sautées pendant la lecture. |
| **Avant** | Le fichier d'origine. |
| **Coupes seules** | Uniquement ce qui sera enlevé, pour vérifier qu'aucun mot n'est perdu. |

Couleurs de la forme d'onde :

| Couleur | Passage |
| --- | --- |
| Vert | Gardé |
| Gris | Silence coupé |
| Orange | Hésitation coupée |
| Rouge | Coupé à la main |

- Un **clic sur la bande colorée** du haut garde un passage coupé, ou coupe un passage gardé.
- Les corrections manuelles sont gardées à part pour chaque fichier.
- La **transcription** surligne le mot en cours de lecture. Un clic sur un mot y place la
  lecture, un Alt+clic garde ou coupe le mot.

### 4. Exporter

Choisissez le logiciel (Premiere, Resolve ou les deux), puis le mode :

- **une séquence** : tous les fichiers à la suite, avec un marqueur au début de chacun ;
- **une séquence par fichier**.

Vous pouvez aussi choisir l'ordre (nom, date de tournage, liste) et le dossier de sortie.

| Logiciel | Import du fichier |
| --- | --- |
| Premiere Pro | **Fichier > Importer**, fichier `.premiere.xml` |
| DaVinci Resolve | **Fichier > Importer > Timeline**, fichier `.resolve.xml` |

### Raccourcis

| Touche | Action |
| --- | --- |
| Espace | lecture / pause |
| ← → | reculer / avancer d'1 s (Maj : 5 s) |
| N / P | coupe suivante / précédente |
| X | garder ou couper le passage sous la tête de lecture |
| M | changer de mode (après / avant / coupes seules) |
| + − 0 | zoom avant / arrière / tout afficher |
| ↑ ↓ | fichier précédent / suivant |
| E | exporter |
| ? | aide |

### Confidentialité

L'application n'ouvre aucun port réseau. Aucune donnée ne sort de l'ordinateur. Seul le premier
usage des hésitations accède au réseau, pour télécharger Whisper.

## Hésitations

La détection des « euh » et des syllabes étirées (« deeee », « doooonc ») est optionnelle. Elle
repose sur deux éléments :

| Élément | Rôle | Où il se trouve |
| --- | --- | --- |
| **Whisper** (`openai/whisper-large-v3-turbo`, licence MIT) | transcrit mot à mot et fournit son encodeur audio, gelé | téléchargé depuis Hugging Face au premier usage (environ 1,6 Go), puis gardé en cache. Il n'est pas dans ce dépôt. |
| **La tête** (petit réseau de neurones) | classe l'audio toutes les 20 ms : parole, hésitation, allongement ou silence | poids inclus dans ce dépôt, dans `python/euhpen/weights/whisper-turbo/` (1,4 Mo). Rien à entraîner. |

C'est le seul modèle proposé, dans l'interface comme avec `euhpen cut --hesitations`.

La tête a été entraînée sur du français. Whisper transcrit bien d'autres langues, mais la
détection n'a été évaluée qu'en français. Les poids sont soumis à des conditions particulières :
pas d'usage commercial, voir [Licence](#licence).

Sur des enregistrements français jamais vus à l'entraînement, le F1 atteint 0,80 pour les « euh »
et 0,58 pour les syllabes étirées ([détail des mesures](#résultats)).

### Utiliser la détection

1. Acceptez l'installation de la détection dans le lanceur, ou installez `python/requirements.txt`.
2. Dans l'interface, activez les hésitations et analysez le fichier. En ligne de commande, ajoutez
   `--hesitations` à `euhpen cut`.

### Calcul

| Machine | Calcul |
| --- | --- |
| Mac Apple Silicon | puce graphique (MPS), float16 |
| Carte NVIDIA | CUDA, float16 |
| Processeur seul | float32, lent |

### Règle de coupe

Pour chaque détection :

1. la coupe va de *début − marge* à *fin + marge*, avec une marge de 0,05 s par défaut ;
2. ses bords sont recalés sur le creux de volume le plus proche, à ±50 ms près ;
3. elle est fusionnée avec les silences voisins.

Whisper colle ses mots les uns aux autres : leurs bornes ne limitent donc pas les coupes, seules
les bornes de la détection comptent.

Dans la transcription, les détections apparaissent sous forme de jetons `[euh]` et `[étirement]`.
Toux, rires et souffles ne sont pas détectés : Whisper ne les écrit pas, et le corpus
d'entraînement ne les annote pas.

La transcription est mise en cache par fichier, modèle et langue. Le nom du cache contient une
empreinte des poids : une tête réentraînée ne réutilise pas d'anciennes détections. Changer les
réglages ne relance rien.

### Limites

- Les allongements restent la classe faible : F1 de 0,58 sur le test interne.
- Un mot entièrement couvert par une détection est coupé avec elle : couper un « deeee » en entier
  peut donc emporter le « de ».
- Whisper nettoie souvent les faux départs. Un mot qu'il n'écrit pas n'est pas protégé : un court
  segment gardé entre deux coupes peut alors être retiré par `min_keep`.
- La précision des horodatages de Whisper autour des pauses n'a pas été mesurée. Elle joue sur les
  bornes des coupes.
- L'entraînement repose sur un peu plus de 3 h d'un seul corpus. La tête n'a été évaluée qu'en
  français, et son comportement sur d'autres langues est inconnu.
- L'entraînement et l'évaluation ont été faits sur Mac (MPS) uniquement.

Le fonctionnement du réseau, son entraînement et les mesures sont décrits dans
[Le modèle en détail](#le-modèle-en-détail).

## Ligne de commande

Avec l'installateur, la commande (`euhpen.exe` sous Windows, `euhpen` sous macOS) est dans le
dossier d'installation. Après une compilation, la
commande est dans `target/release/` (`target\release\euhpen.exe` sous Windows). Laissez-la dans le dossier d'Euhpen : c'est là qu'elle trouve le modèle, sauf si la
variable `EUHPEN_ROOT` indique un autre emplacement.

```bash
euhpen cut interview.mov                      # silences → interview_euhpen.{premiere,resolve}.xml
euhpen cut interview.mov --hesitations        # + « euh » (langue : --language fr)
euhpen cut rushes/ --order date --preset Podcast
euhpen cut interview.mov --dry-run --min-silence 0.6   # statistiques sans rien écrire
```

```
Fichier            Avant   Après  Supprimé  Coupes       Seuil  Euh coupés
────────────────  ──────  ──────  ────────  ──────  ──────────  ──────────
interview fr.mov  0:19,7  0:11,4    42,1 %      10  -37,1 dBFS           6
```

Plusieurs fichiers (ou un dossier) sont mis à la suite dans une seule timeline, avec un
marqueur au début de chacun.

### Réglages

| Option | Défaut | Rôle |
| --- | --- | --- |
| `--threshold` | `auto` | Seuil en dBFS (ex. `-40`) ou `auto` : calculé entre le bruit de fond et le niveau de la parole de chaque fichier |
| `--min-silence` | `0.4` | Durée minimale (s) d'un silence à couper ; les pauses plus courtes restent pour garder un rythme naturel |
| `--pad-before` | `0.1` | Air gardé (s) avant la reprise de la parole |
| `--pad-after` | `0.15` | Air gardé (s) après la fin de la parole |
| `--min-keep` | `0.25` | Un segment gardé plus court est supprimé (évite les micro-coupes) |
| `--min-sound` | `0.05` | Un son isolé plus court (clic, claquement) compte comme du silence |
| `--hysteresis` | `3` | La parole ne s'arrête que sous (seuil − hystérésis) : la détection n'oscille pas |
| `--hesitations` | non | Coupe les « euh » et les syllabes étirées (Whisper + tête) |
| `--language` | `fr` | Langue parlée (24 langues ; la tête n'a été évaluée qu'en français) |
| `--event-margin` | `0.05` | Marge (s) autour d'une hésitation |
| `--preset NOM` | | Part d'un preset (ceux de l'interface, y compris les vôtres) |
| `--settings fichier.json` | | Charge un jeu de réglages (mêmes noms que ci-dessus, avec `_`) |

### Presets fournis

| Preset | Pour |
| --- | --- |
| Standard | silences de plus de 0,4 s, air naturel autour de la parole |
| Sans hésitations | comme Standard, et coupe les « euh » et les syllabes étirées |
| Dynamique (YouTube) | rythme serré : presque tous les blancs et les hésitations |
| Interview posée | ne coupe que les vrais blancs, garde de l'air |
| Podcast | ne coupe que les longs silences |

Vos propres presets s'enregistrent depuis l'interface. `euhpen cut --help` liste toutes les
options, dont le logiciel visé, l'ordre des fichiers et le nom de la séquence.

### Autres commandes

```bash
euhpen probe rush.mov       # fps exact, timecode, pistes audio, fréquence variable…
euhpen analyze rushes/      # analyse et met en cache sans exporter
euhpen cfr telephone.mp4    # convertit une vidéo à fréquence variable (voir plus bas)
euhpen cache clear          # vide le cache d'analyse
euhpen train CORPUS         # entraîne la tête française, voir « Réentraîner »
```

## Bon à savoir

- **Fréquence d'images variable** (smartphones, OBS) :
  - Les logiciels de montage supposent une fréquence fixe, et les coupes se décalent peu à peu.
  - Euhpen le détecte et vous avertit.
  - `euhpen cfr fichier.mp4`, ou le bouton « Convertir » de l'interface, crée une copie à
    fréquence fixe (`fichier_cfr.mov`, H.264) à utiliser à la place.
- **Calage sur les images** :
  - Toutes les coupes tombent sur des images entières.
  - Elles sont calculées avec la fréquence exacte en fraction (30000/1001 et non 29,97). Ainsi
    l'image et le son restent synchrones, même à la fin d'un fichier d'une heure.
  - Au calage, un segment ne peut que s'allonger : il ne rogne jamais un mot.
- **Pistes audio** :
  - Variante Premiere : une source stéréo donne une paire de pistes, une source mono ou
    multicanal une piste par canal.
  - Variante Resolve : une piste par flux audio.
  - La détection utilise le canal le plus fort. Une caméra dont seul le canal 1 porte le micro
    fonctionne donc.
- **Timecode** :
  - Le timecode de départ des caméras pro, et celui des WAV BWF, est déclaré dans le XML.
  - Si votre logiciel décale les clips, réessayez avec `--zero-timecode` (ou l'option
    équivalente de l'interface) et signalez-le.
- **Chemins** : les fichiers sont référencés par URL `file://` encodée, accents et espaces
  compris. En cas de média hors ligne, essayez `--raw-paths`.
- **Cache** :
  - Il contient l'analyse, l'aperçu et la transcription.
  - Il est indexé par une empreinte du contenu du fichier, pas par son nom : renommer ou
    déplacer un rush conserve son analyse.
  - Son emplacement s'obtient avec `euhpen cache`, et il se vide depuis l'interface (bouton
    Système) ou avec `euhpen cache clear`.

## Architecture

Le principe : séparer l'**analyse** (coûteuse, une fois par fichier) de la **décision**
(fonction pure, instantanée). C'est ce qui permet aux curseurs de recalculer en direct.

```
src/                 tout sauf le modèle, en Rust
  fps.rs             fréquences d'images exactes (fractions), conversions, timecodes
  probe.rs           ffprobe : fps exact (fraction), durée, pistes audio, timecode, VFR
  audio.rs           ffmpeg → enveloppe RMS en dBFS par fenêtres de 10 ms
  analysis.rs        ANALYSE : métadonnées + enveloppe, en cache (empreinte du fichier)
  worker.rs          pilote le processus Python du modèle
  transcript.rs      transcriptions en cache, nature des mots
  settings.rs        réglages de la détection
  intervals.rs       opérations sur des listes d'intervalles
  decision.rs        DÉCISION : seuil, hystérésis, silences, hésitations, marges, corrections
  corrections.rs     corrections manuelles, par fichier
  timeline.rs        calage sur les images, assemblage de plusieurs fichiers
  fcp7.rs            XML Final Cut Pro 7 (variantes Premiere et Resolve)
  proxy.rs           aperçu H.264/AAC léger pour la lecture
  cfr.rs             conversion VFR → fréquence fixe
  presets.rs         presets fournis et presets de l'utilisateur
  cache.rs           dossier de cache, empreinte rapide des fichiers
  config.rs          préférences de l'utilisateur
  files.rs           fichiers d'entrée et de sortie
  cli.rs             ligne de commande
  gui/               interface native (egui)
    state.rs           état de l'application, travaux en arrière-plan
    player.rs          lecture : horloge audio (cpal), images décodées par ffmpeg
    panels.rs          rushes, réglages, export
    preview.rs         aperçu et transport
    timeline.rs        forme d'onde, zoom, corrections
    words.rs           transcription cliquable
    dialogs.rs         fenêtres et messages
    theme.rs           couleurs et réglages visuels
    selftest.rs        autotest sans intervention, avec captures d'écran
  setup/             installateur Windows et macOS (euhpen-setup)
    wizard.rs          les pages de l'assistant
    install.rs         installation, mise à jour, désinstallation, communes aux deux systèmes
    job.rs             avancement, téléchargements vérifiés, processus
    windows.rs         propre à Windows : registre, raccourcis, téléchargements
    macos.rs           propre à macOS : paquets .app, raccourci, téléchargements
  bin/               points d'entrée de la ligne de commande, de l'interface et de l'installateur
python/euhpen/       le modèle, en Python
  worker.py          processus séparé : reçoit l'audio décodé, renvoie les mots (JSON sur stdio)
  transcribe.py      transcription mot à mot Whisper
  weights/           poids de la tête française (CC BY-NC-SA 3.0 FR)
  finetune/          tête de détection française
    corpus.py          Rhapsodie → étiquettes par trame, découpage sans locuteur nommé commun
    encoder.py         encodeur gelé de Whisper → états de 1280 dimensions
    features.py        décodage en 16 kHz, cache disque des états
    head.py            réseau, sauvegarde, chargement, inférence
    train.py           boucle d'entraînement, calibration des seuils
    spans.py           probabilités → intervalles, insertion dans la transcription, scores
    pipeline.py        transcription et détections, en cache
    corrections.py     journal local des corrections de l'interface
    holdout.py         refuse le fichier réservé à l'évaluation
packaging/           ce qui est propre à chaque système, hors code de l'application
  windows/             lanceur et construction de l'installateur (.exe)
  macos/               lanceur, construction de l'installateur (.dmg), descriptions des paquets .app
  notes-de-version.md  texte des versions publiées
Lancer Euhpen.bat    double-clic sous Windows
Lancer Euhpen.command  double-clic sous macOS
tests/               générateur de mires pour tester l'import
build.rs             bibliothèque d'import OpenGL (chaîne Windows), application embarquée dans l'installateur
```

Points d'architecture :

- Les segments sont décrits par `{source_in, source_out, timeline_start}` en images.
- Les corrections manuelles (forcer à garder ou à couper) sont stockées à part, puis appliquées
  par-dessus le calcul automatique.
- Les horodatages des mots sont en temps audio. Ils sont décalés du `audio_offset` du fichier
  pour rester calés sur la vidéo.

### Pourquoi un processus Python ?

Whisper et la tête tournent avec PyTorch et transformers, qui n'existent qu'en Python. Ce code
est isolé dans un processus démarré à la demande : Rust lui envoie l'audio déjà décodé, et il
répond par lignes JSON. Annuler une transcription revient à tuer ce processus. Le modèle et la
tête sont chargés sur le même matériel, CUDA dès qu'il est disponible.

L'interface et l'aperçu sont natifs. La vidéo est décodée par ffmpeg et affichée directement,
sans moteur web : le son donne l'horloge, et l'image suit.

## Le modèle en détail

### Quel type de réseau ?

C'est un **réseau convolutif à une dimension (CNN 1D)**, plus précisément un **réseau temporel
convolutif résiduel à convolutions dilatées**, de la famille des TCN. Il fait de la
**classification trame par trame** : il sort autant de prédictions qu'il reçoit de trames. Il est
posé sur un **Transformer gelé**, l'encodeur de Whisper large-v3-turbo (32 couches, états de 1280
dimensions). Les poids de Whisper ne changent pas : seule la tête est apprise.

```
audio → ffmpeg, mono 16 kHz → fenêtres de 30 s
  ↓
encodeur Whisper large-v3-turbo, gelé                (trames, 1280), une trame toutes les 20 ms
  ↓
LayerNorm → Linear 1280→96 → GELU → Dropout 0,2      (trames, 96)
  ↓
4 blocs convolutifs résiduels, dilatations 1, 2, 4, 8 (trames, 96)
  ↓
Linear 96→4 → softmax                                parole, hésitation, allongement, silence
  ↓
filtre médian, seuils, fusion, durée minimale        intervalles
  ↓
jetons [euh] et [étirement] dans la transcription → decide() → coupes
```

Chaque bloc applique ceci, où `d` est la dilatation :

```
y = Conv1d(96→96, noyau 5, dilatation d, padding 2d) → GELU → Dropout 0,2 → Conv1d(96→96, noyau 1)
sortie = GroupNorm(8 groupes, x + y)
```

| Partie | Détail | Paramètres |
| --- | --- | ---: |
| Normalisation d'entrée | `LayerNorm(1280)` | 2 560 |
| Projection | `Linear(1280, 96)` | 122 976 |
| 4 blocs | 55 680 chacun | 222 720 |
| Sortie | `Linear(96, 4)` | 388 |
| **Total** | | **348 644** |

**Ce que voit le réseau.** Noyau de 5 et dilatations 1, 2, 4, 8 donnent un champ récepteur de
1 + 4 × (1 + 2 + 4 + 8) = 61 trames, soit 1,22 s : environ 0,6 s de chaque côté de la trame. Les
convolutions sont centrées, donc le réseau regarde aussi le futur. Il n'est pas conçu pour le
temps réel.

**Pourquoi ce découpage.** L'encodeur fournit déjà, pour chaque trame, une représentation qui
tient compte de jusqu'à 30 s d'audio. Reste à décider à l'échelle de la syllabe, ce qu'une
convolution dilatée couvre avec peu de paramètres : un avantage avec seulement 2,2 h
d'entraînement. Cette architecture a été choisie a priori. Aucune autre n'a été essayée.

**Lecture des longs fichiers.** L'encodeur lit des fenêtres de 30 s qui se recouvrent de 6 s. Les
3 s de chaque bord sont jetées, sauf au début et à la fin du fichier, car elles disposent de moins
de contexte. Les états sont gardés en `float16`.

### Comment la tête a été entraînée

**Données.** [Rhapsodie](https://rhapsodie.modyco.fr) seul : du français parlé aligné à la main
au phonème. L'entraînement porte sur la tête uniquement, à partir d'états de l'encodeur calculés
une fois pour toutes.

Rhapsodie marque les syllabes disfluentes par `H` sans dire de quelle disfluence il s'agit. Les
étiquettes, une par trame de 20 ms, sont donc déduites ainsi :

| Classe | Règle |
| --- | --- |
| `hesitation` | tout le mot, s'il s'agit de euh, heu, euhm, hum ou hm |
| `lengthening` | dans une syllabe `H`, **seulement l'excès** du phonème le plus long |
| `silence` | phonème de pause |
| `speech` | autres phonèmes |
| ignorée (`-1`) | bruits, syllabes douteuses, « mh » d'écoute, parole superposée, hors annotation |

Le phonème le plus long (voyelle ou continue) compte comme allongé s'il dure au moins 0,18 s et
au moins deux fois la médiane du fichier pour sa classe de sons. L'excès commence après
max(0,08 s ; médiane) et doit durer au moins 0,06 s. Le début du son reste ainsi étiqueté parole,
puisque c'est la partie à garder au montage. Ces valeurs ont été fixées a priori.

**Découpage.** Il est déterministe, par hachage des enregistrements. Les enregistrements qui
partagent un locuteur nommé dans les métadonnées restent dans la même partie (`SPEAKER_GROUPS`).
Des locuteurs anonymes communs restent possibles.

| Partie | Enregistrements | Durée | `speech` | `hesitation` | `lengthening` | `silence` | « euh » | Allongements |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Entraînement | 41 | 2,24 h | 270 053 | 11 874 | 7 958 | 102 031 | 639 | 616 |
| Validation | 8 | 0,40 h | 49 665 | 3 215 | 2 297 | 15 519 | 157 | 156 |
| Test interne | 8 | 0,51 h | 58 418 | 4 474 | 3 267 | 12 634 | 217 | 245 |

Les quatre colonnes de classes comptent des trames de 20 ms.

**Boucle d'apprentissage** (`python/euhpen/finetune/train.py`) :

| Élément | Valeur |
| --- | --- |
| Exemples | extraits aléatoires de 10 s (500 trames), tirés proportionnellement à la longueur des enregistrements |
| Lots | 32 extraits, soit 25 pas par époque |
| Perte | entropie croisée, trames `-1` ignorées |
| Poids de classes | inverse de la racine de la fréquence : parole 0,30, hésitation 1,44, allongement 1,76, silence 0,49 |
| Optimiseur | AdamW, `weight_decay` 0,01 |
| Taux d'apprentissage | `OneCycleLR`, maximum 2·10⁻³, prévu sur 40 époques |
| Arrêt | au plus 40 époques, patience de 8 sur la validation |
| Critère | moyenne des F1 par trame des classes hésitation et allongement |
| Graine | 7 |
| Matériel | CUDA, sinon MPS, sinon processeur |

Les poids fournis ont été entraînés sur MPS, sans corrections de l'interface. L'arrêt anticipé a
eu lieu à l'époque 15, avant la fin du cycle du taux d'apprentissage. Le meilleur état, conservé,
est celui de l'époque 7, avec un F1 par trame de 0,717 en validation.

**Seuils.** Une fois le réseau figé, chaque seuil est cherché sur la validation seule, de 0,20 à
0,90 par pas de 0,05, pour maximiser le F1 par événement. Le test interne n'intervient ni ici ni
dans l'arrêt anticipé.

| Classe | Seuil | Précision | Rappel (validation) |
| --- | ---: | ---: | ---: |
| Hésitation | 0,70 | 0,90 | 0,88 |
| Allongement | 0,45 | 0,68 | 0,60 |

### Des probabilités aux coupes

Les réglages sont enregistrés dans `head.json` et appliqués dans cet ordre :

1. filtre médian sur 5 trames (100 ms) des probabilités d'hésitation et d'allongement ;
2. hésitation si p ≥ 0,70 et p ≥ allongement ; allongement si p ≥ 0,45 et p > hésitation ;
3. fusion des détections d'une même classe séparées de 60 ms au plus ;
4. retrait des hésitations de moins de 120 ms et des allongements de moins de 80 ms.

`insert_spans` ajoute ensuite les détections à la transcription de Whisper, sous forme de jetons
`[euh]` pour les hésitations et `[étirement]` pour les allongements. Ce marquage reste aussi fiable
que la distinction faite par la tête, mais il ne change rien aux coupes : les deux sont traités
comme des hésitations.

- une détection déjà couverte à moitié par un « euh » écrit par Whisper est ignorée ;
- un mot touché en partie est scindé, et un morceau de moins de 30 ms est abandonné ;
- un mot entièrement couvert disparaît au profit du jeton.

`decide()` applique ensuite la [règle de coupe](#règle-de-coupe). Un segment gardé trop court pour
`min_keep` n'est pas retiré s'il contient le milieu d'un mot entier : la tête coupe à l'intérieur
des mots, et ce qui reste entre deux coupes est souvent un mot bref comme « de » ou « un ».

### Résultats

Une détection compte comme juste si elle recouvre une annotation sur au moins la moitié du plus
court des deux intervalles, avec un appariement un à un.

**Test interne Rhapsodie** (8 enregistrements jamais vus, annotation manuelle, 217 « euh », 245
allongements) :

| Catégorie | Précision | Rappel | F1 |
| --- | ---: | ---: | ---: |
| « euh » | 0,81 | 0,79 | 0,80 |
| Allongements | 0,60 | 0,56 | 0,58 |
| Tout | 0,72 | 0,71 | 0,72 |

**Chaîne complète sur ce même test** (poids et seuils figés, seuil auto, marge 0 s) :

| Mesure | Valeur |
| --- | ---: |
| Part moyenne retirée de chaque « euh » | 76 % |
| « euh » retirés à 80 % ou plus | 71 % |
| Part moyenne retirée de chaque allongement | 55 % |
| Secondes coupées par la tête | 153,2 |
| dont hors passages annotés | 44,5 |

Les passages hors annotations ne sont pas forcément des mots coupés : Rhapsodie n'annote pas tout,
et les silences voisins y comptent.

**Fichier de test réservé** (35 s, un locuteur, 5 « euh », 8 allongements) : 5 « euh » sur 5,
7 allongements sur 8, 3 fausses alertes (tout : précision 0,80, rappel 0,92, F1 0,86). En
évaluation seulement, 15 coupes pour 7,5 s retirées, et 92 à 98 % de la durée de chaque « euh ».

Ce dernier résultat est à lire avec prudence. Sa référence a été établie à partir de spectrogrammes
et de transcriptions, sans écoute. Les 13 événements viennent d'un seul locuteur. Le fichier n'a
servi ni à entraîner ni à régler quoi que ce soit, et le code le refuse à l'entraînement et dans le
journal des corrections, par nom et par empreinte de contenu (`python/euhpen/finetune/holdout.py`). Il a
toutefois servi à repérer un défaut de la règle de coupe : il n'est plus une mesure à l'aveugle
pour cette règle.

### Réentraîner

Ce n'est pas nécessaire pour utiliser la détection. Pour produire vos propres poids, téléchargez
[Rhapsodie](https://rhapsodie.modyco.fr) (CC BY-NC-SA 3.0 FR). Euhpen y cherche récursivement les
fichiers `*-Pro.TextGrid` et le `.wav` du même nom, par exemple `Rhap-D0001-Pro.TextGrid` et
`Rhap-D0001.wav`. Puis :

```bash
euhpen train /chemin/vers/rhapsodie --no-corrections --output /chemin/vers/tete
```

| Option | Défaut | Rôle |
| --- | --- | --- |
| `--output` | dossier de configuration | Dossier des poids |
| `--no-corrections` | corrections incluses | N'utilise que Rhapsodie |
| `--epochs` | `40` | Nombre maximal d'époques |

Sans `--output`, les poids sont écrits dans le dossier de configuration et **remplacent ceux
fournis avec Euhpen** sur cette machine. Pour revenir aux poids fournis, supprimez `head.pt` et
`head.json` de ce dossier :

| Système | Dossier |
| --- | --- |
| macOS | `~/Library/Application Support/euhpen/finetune/whisper-turbo/` |
| Windows | `%APPDATA%\euhpen\finetune\whisper-turbo\` |
| Linux | `~/.config/euhpen/finetune/whisper-turbo/` |

La variable `EUHPEN_CONFIG_DIR` remplace le dossier de configuration.

Les états de l'encodeur sont mis en cache dans `<cache>/finetune/features/<modèle>/` (environ
1,5 Go pour tout le corpus). La graine est fixe, mais rien ne garantit des poids identiques d'une
machine à l'autre.

**Corrections de l'interface.** Les coupes et les annulations sont notées, sans rien envoyer, dans
`<dossier de configuration>/finetune/corrections.jsonl`. Seules deux corrections deviennent des
étiquettes d'entraînement, car elles disent clairement ce que contient le passage :

| Correction | Étiquette |
| --- | --- |
| garder un passage coupé comme hésitation | parole |
| couper un mot précis (1,5 s au plus) | hésitation |

Les autres trames de ces fichiers sont ignorées. Par défaut, `euhpen train` ajoute ces corrections
à l'entraînement et recalcule les états des fichiers concernés, qui doivent encore exister sur le
disque.

### Réutiliser le réseau dans un autre projet

Le réseau tient dans `python/euhpen/finetune/` : `head.py`, `encoder.py`, `spans.py` pour l'inférence,
`corpus.py`, `features.py`, `train.py` pour l'entraînement. Le code est sous licence MIT, Whisper
aussi. Les poids de `python/euhpen/weights/whisper-turbo/` sont sous CC BY-NC-SA 3.0 FR : un projet qui
les reprend doit créditer Rhapsodie, rester non commercial et partager ses dérivés sous la même
licence. Voir [Licence](#licence).

Depuis Python (avec `python/` dans le `PYTHONPATH`), sans passer par la transcription ni par les coupes :

```python
from euhpen.finetune.head import detect

for span in detect("audio.wav"):  # poids fournis avec Euhpen
    print(span.start, span.end, span.kind, span.probability)  # kind : hesitation ou lengthening
```

## Tests

```bash
cargo test
cd python
python -m pip install -r requirements.txt pytest ruff
python -m pytest
python -m ruff check euhpen tests
```

`cargo test` couvre le calcul des coupes (silences, hésitations, marges, corrections manuelles),
le calage sur les images, le XML, les réglages, la lecture qui saute les coupes et la forme d'onde.
Il n'a besoin ni de ffmpeg ni du modèle.

Les tests Python couvrent la tête : étiquettes, découpage sans locuteur commun, refus du fichier
réservé, journal des corrections, passage des probabilités aux coupes, cache et protection des
mots courts. Ils n'utilisent ni les poids entraînés ni le corpus.

Pour contrôler l'application entière sans intervention :

```bash
EUHPEN_SELFTEST=sortie EUHPEN_SELFTEST_FILES=rush.mov target/release/euhpen-gui
```

Elle charge le fichier, joue, se déplace, ouvre les fenêtres, exporte, puis se ferme. Le dossier
`sortie` reçoit `selftest.log` et des captures d'écran. Avec `EUHPEN_SELFTEST_HES=1`, elle passe
aussi par la transcription.

Pour construire l'installateur (`dist\Euhpen-Setup-<version>.exe`), sous Windows, sans Visual
Studio ni MinGW :

```bash
powershell -NoProfile -ExecutionPolicy Bypass -File packaging/windows/creer-installateur.ps1
```

Sous macOS, sur une puce Apple (`dist/Euhpen-Setup-<version>-macos-arm64.dmg`) :

```bash
bash packaging/macos/creer-installateur.sh
```

Pousser un tag `vX.Y.Z` égal à la version de `Cargo.toml` construit les deux installateurs sur
GitHub Actions et publie la version (`.github/workflows/release.yml`).

Pour l'essayer sans toucher au système, `EUHPEN_SETUP_SANDBOX=<dossier>` isole l'installation,
les raccourcis et l'inscription, et `EUHPEN_SETUP_AUTOTEST=<dossier>` parcourt seul les pages en
prenant des captures. Ses tests unitaires : `cargo test --features installer --lib setup::`.

Pour tester l'import dans Premiere et Resolve, `python tests/make_test_media.py DOSSIER`
produit une mire. C'est une vidéo dont l'image passe au blanc exactement quand un bip démarre,
avec des silences de durées connues. Après import, chaque reprise doit montrer le flash et le
début du bip sur la même image, y compris à la fin de la version longue (`--long`, 1 h). La
liste des vérifications est dans [docs/tests-import.md](docs/tests-import.md).

## État et limites

État de la version 0.1.0.

**Vérifié sous Windows 11, carte NVIDIA RTX 5070** :

- les tests Rust et Python ;
- le calcul des coupes et les XML, comparés à une implémentation de référence : 600 jeux de
  réglages aléatoires donnent les mêmes coupes, et les XML de deux médias sont identiques ;
- la transcription sur CUDA, depuis l'interface et en ligne de commande ;
- l'interface, par l'autotest : analyse, aperçu image et son, lecture qui saute les coupes,
  saut dans le fichier, inversion d'une coupe, fenêtres, export ;
- le lanceur Windows sur une copie vierge du projet (compilation complète, sans MinGW ni Visual
  Studio ; réparation d'un PyTorch sans CUDA, simulée) et sur un environnement réel avec CUDA ;
- l'installateur, dans un dossier isolé et avec les vrais téléchargements : installation complète
  puis transcription sur CUDA, installation sans carte NVIDIA (simulée) dans un dossier avec
  espace et accent, coupure du réseau et reprise, mise à jour, Euhpen resté ouvert, annulation,
  désinstallation.

**Vérifié sur un Mac à puce Apple** :

- les tests Rust, la compilation et la construction de l'image disque ;
- l'image disque publiée, dans un dossier isolé et avec les vrais téléchargements, sans ffmpeg ni
  Whisper déjà présents : installation complète, désinstallation ;
- sans la détection des hésitations : installation, mise à jour par-dessus, désinstallation ;
- l'application ainsi installée, ouverte avec un `PATH` réduit comme depuis le Finder :
  transcription sur MPS, lecture, aperçu de l'image, inversion d'une coupe, export, ligne de commande.

**Pas encore vérifié avec cette version** :

- sous macOS, le double-clic sur l'installateur depuis l'image disque après l'alerte de macOS, et
  le lanceur `Lancer Euhpen.command` ;
- la lecture de longs fichiers en conditions réelles, et le rendu sur d'autres cartes son ;
- l'import réel dans Premiere Pro et DaVinci Resolve, dont la synchronisation sur 1 h (liste dans
  [docs/tests-import.md](docs/tests-import.md)) ;
- Linux, couvert seulement par l'intégration continue ;
- l'installateur sur un Windows vierge, sur une machine réellement sans carte NVIDIA, et dans son
  dossier par défaut.

Hors périmètre : multicam, synchronisation, locuteur actif, export CapCut, rendu MP4 direct.

## Licence

Code sous licence **MIT** (voir [LICENSE](LICENSE)).

La détection des hésitations s'appuie sur deux éléments qui ne sont pas couverts par cette licence :

- **Whisper** (`openai/whisper-large-v3-turbo`, OpenAI, licence MIT) n'est ni embarqué ni
  rehébergé : il est téléchargé depuis Hugging Face à l'installation ou au premier usage.
- **Les poids de la tête** (`python/euhpen/weights/whisper-turbo/`) sont entraînés sur le corpus
  [Rhapsodie](https://rhapsodie.modyco.fr) et distribués sous la même licence,
  [CC BY-NC-SA 3.0 FR](https://creativecommons.org/licenses/by-nc-sa/3.0/fr/) : attribution, pas
  d'usage commercial, partage à l'identique. Détails dans
  [LICENCE.md](python/euhpen/weights/whisper-turbo/LICENCE.md).

**Un usage commercial de la détection des hésitations (vidéos monétisées, travail pour des
clients) n'est donc pas couvert par ces poids.** La détection des silences ne les utilise pas et
reste entièrement sous licence MIT. Le code d'entraînement et d'inférence est sous licence MIT.

La fiche Hugging Face de Rhapsodie indique CC0 : la licence retenue ici est celle du site du
corpus.

ffmpeg n'est pas fourni avec Euhpen : il s'installe séparément, ou l'installateur le télécharge
à part (sous licence GPL : version « essentials » de gyan.dev sous Windows, compilation de
martin-riedl.de sous macOS). Il reste sous sa propre licence.
