# Tests d'import dans Premiere Pro et DaVinci Resolve

Les tests automatiques (`pytest`) vérifient le XML produit, mais seul le logiciel de montage
dit s'il l'accepte. Cette liste se fait à la main, à chaque changement de l'export et à chaque
nouvelle version de Premiere ou de Resolve.

## 1. Préparer les fichiers de test

```bash
python tests/make_test_media.py ~/EuhpenTests --long
for f in ~/EuhpenTests/*.mov ~/EuhpenTests/*.mp4 ~/EuhpenTests/*.wav; do euhpen cut "$f" -o ~/EuhpenTests/xml/; done
euhpen cut ~/EuhpenTests -o ~/EuhpenTests/xml/lot.xml
```

Sous Windows (PowerShell) :

```powershell
python tests\make_test_media.py $HOME\EuhpenTests --long
Get-ChildItem $HOME\EuhpenTests\* -Include *.mov,*.mp4,*.wav | ForEach-Object { euhpen cut $_.FullName -o $HOME\EuhpenTests\xml\ }
euhpen cut $HOME\EuhpenTests -o $HOME\EuhpenTests\xml\lot.xml
```

Chaque fichier coupé seul donne `<nom>_euhpen.premiere.xml` et `<nom>_euhpen.resolve.xml`.
Plusieurs fichiers (ou un dossier) dans une même commande donnent **une seule** timeline,
avec un marqueur par fichier : c'est `lot.premiere.xml` / `lot.resolve.xml`.

**Ce que contiennent les fichiers.** Une mire avec un compteur, et un son de 220 Hz par
salves, sur un motif de 12 s répété : son de 1,0 à 3,0 s, de 3,3 à 5,0 s, de 6,0 à 6,5 s et de
8,5 à 11,0 s, silence ailleurs. **L'image où commence chaque salve est entièrement blanche.**
C'est le repère de synchro : dans le logiciel de montage, le flash blanc doit tomber sur
l'image où la forme d'onde démarre, 3 ou 4 images après chaque coupe (marge de 0,1 s).

Une période de 12 s donne 3 segments gardés (la pause de 0,3 s à 3,0 s est trop courte pour
être coupée). Un fichier de 36 s donne donc **9 clips**.

## 2. Valeurs attendues (réglages par défaut)

| Fichier | Séquence | Clips | Durée séquence | 1er clip (source, images) | TC source du 1er clip |
|---|---|---|---|---|---|
| 23976 mono.mov | 23,976 | 9 | 566 images | 21 → 124 | 00:00:00:21 |
| 25 mono.mov | 25 | 9 | 588 images | 22 → 129 | 00:00:00:22 |
| 2997 stéréo.mov | 29,97 | 9 | 705 images | 26 → 155 | 00:00:00:26 |
| 50 quatre canaux.mov | 50 | 9 | 1167 images | 45 → 258 | 00:00:00:45 |
| 5994 mono.mov | 59,94 | 9 | 1402 images | 53 → 309 | 00:00:00:53 |
| 25 timecode 10h.mov | 25 | 9 | 588 images | 22 → 129 | 10:00:00:22 |
| 2997 timecode DF.mov | 29,97 DF | 9 | 705 images | 26 → 155 | 01:00:00;26 |
| Prise été n°1 — accents & espaces.mov | 25 | 9 | 588 images | 22 → 129 | 00:00:00:22 |
| obs deux pistes.mov | 30 | 9 | 702 images | 27 → 155 | 00:00:00:27 |
| micro BWF 10h.wav | 25 (audio seul) | 9 | 588 images | 22 → 129 | 10:00:00:22 |
| 2997 une heure.mov (`--long`) | 29,97 | 900 | 70578 images | 26 → 155 | 00:00:00:26 |

« 1er clip 21 → 124 » : point d'entrée à l'image 21 de la source, sortie à l'image 124
(exclue). La séquence commence à 00:00:00:00 sans drop-frame : la séquence d'une heure dure
donc 00:39:12:18 à l'affichage.

Dans le fichier d'une heure, le dernier clip va de l'image source 107784 à 107867, et le flash
est l'image 107788 (TC source 00:59:52:28, soit 59 min 56,5 s de temps réel) : c'est là
qu'une dérive se verrait. Euhpen a vérifié avec ffmpeg que cette image est bien blanche ; il
reste à vérifier que le logiciel de montage lit la même.

`téléphone VFR.mp4` (fréquence variable) doit afficher un avertissement à la coupe. On le
convertit d'abord : `euhpen cfr "téléphone VFR.mp4"`, puis on coupe `téléphone VFR_cfr.mov`.

## 3. Premiere Pro

Fichier › Importer, choisir le `.premiere.xml`. Pour chaque fichier, cocher :

- [ ] Import sans message d'erreur ; une séquence au nom du fichier apparaît.
- [ ] Séquence › Paramètres de la séquence : fréquence attendue (tableau ci-dessus).
- [ ] Média en ligne (pas d'écran « Média hors ligne »), y compris le fichier à accents.
- [ ] Nombre de clips et durée de la séquence conformes, clips bout à bout sans trou.
- [ ] Sur plusieurs coupes au début, au milieu et à la fin : le flash blanc tombe sur
      l'image où démarre la forme d'onde.
- [ ] Vidéo et audio liés : cliquer la vidéo sélectionne aussi l'audio.
- [ ] Audio : `2997 stéréo` s'entend sur les deux canaux ; `50 quatre canaux` occupe 4 pistes
      mono ; `obs deux pistes` occupe 2 pistes.
- [ ] Timecode : le moniteur source du 1er clip affiche le TC du tableau (10:00:00:22,
      01:00:00;26 avec points-virgules pour le drop-frame).
- [ ] Fichier d'une heure : le flash du dernier clip est toujours calé sur la forme d'onde.
- [ ] Timeline à plusieurs fichiers (`lot.xml`) : un marqueur au nom de chaque fichier à son
      premier clip ; les fichiers d'une autre fréquence sont convertis par Premiere.

## 4. DaVinci Resolve

Fichier › Importer › Timeline (AAF, EDL, XML…), choisir le `.resolve.xml`. Dans la fenêtre
d'import, laisser « Importer automatiquement les clips sources dans le panneau média » coché.
Dans un projet vide, laisser aussi Resolve régler la fréquence du projet sur celle du XML.

- [ ] Import sans erreur ; la timeline apparaît dans le panneau média.
- [ ] Fréquence de la timeline conforme (clic droit sur la timeline › Paramètres de la timeline).
- [ ] Aucun clip rouge (média hors ligne), y compris le fichier à accents.
- [ ] Nombre de clips et durée conformes, sans trou.
- [ ] Flash blanc calé sur la forme d'onde au début, au milieu et à la fin (fichier d'une heure).
- [ ] Vidéo et audio liés (le maillon est actif ; déplacer la vidéo déplace l'audio).
- [ ] Audio : une piste par piste du fichier : `2997 stéréo` = 1 piste stéréo,
      `50 quatre canaux` = 1 piste à 4 canaux, `obs deux pistes` = 2 pistes.
- [ ] Timecode source du 1er clip conforme au tableau.
- [ ] Timeline à plusieurs fichiers : marqueurs présents.

## 5. Chemins

- [ ] macOS : dossier avec espaces et accents (le fichier `Prise été n°1 — …`).
- [ ] Windows : générer les fichiers et lancer `euhpen cut` sous Windows, importer dans
      Premiere et Resolve sous Windows (URL `file://localhost/C:/…`).
- [ ] Windows, disque réseau `\\serveur\partage\…` (URL `file://serveur/partage/…`).
- [ ] Fichier déplacé après la coupe : le logiciel le signale hors ligne et la reconnexion
      manuelle fonctionne (le XML contient des chemins absolus, c'est attendu).

## 6. Si quelque chose ne va pas

- **Clips décalés, noirs ou hors ligne sur les fichiers avec timecode** : refaire l'export avec
  `--zero-timecode`. Si cela corrige le problème, le noter (logiciel, version, fichier).
- **Média hors ligne alors que le fichier existe** : refaire l'export avec `--raw-paths`.
- **Désynchronisation qui grandit au fil du fichier** : vérifier que le fichier n'est pas VFR
  (`euhpen probe fichier`) et le convertir avec `euhpen cfr`.

Pour signaler un problème : système, logiciel et version, fichier de test, cible
(`premiere` ou `resolve`), ce qui est observé, et le XML joint.

## Résultats

| Date | Système | Logiciel et version | Résultat | Notes |
|---|---|---|---|---|
| | | | | |
