# Spike Slint 1.18.1 : validation du backend (ADR 0001, section 7)

Code jetable. Il teste cinq critères sous Windows 10, en build release.

```powershell
$env:CARGO_TARGET_DIR = 'C:\path\to\pistolame\target'
cargo build --release -p spike-slint-backend --manifest-path prototypes/Cargo.toml
& "$env:CARGO_TARGET_DIR\release\spike-slint-backend.exe"
```

Options : `--rows N` (défaut 100000, `0` pour la base de RAM), `--no-spectrogram`,
`--autobench` (tri, filtre et défilement sans intervention, résultats sur la sortie standard).
`measure.ps1` échantillonne la RAM et le CPU du binaire release.

## Crates

| Crate | Version | Licence | Rôle |
|---|---|---|---|
| `slint` + `slint-build` | =1.18.1 (`unstable-winit-030`, `raw-window-handle-06`) | GPL-3.0-only OR royalty-free OR commerciale | UI, drop de fichiers, HWND, `SystemTrayIcon` intégré |
| `souvlaki` | 0.8.3 | MIT | SMTC (touches média) |
| `raw-window-handle` | 0.6 | MIT OR Apache-2.0 | lire le HWND |

Pas de crate `tray-icon` : Slint 1.18 a un `SystemTrayIcon` intégré.

## Checklist manuelle (à faire par toi)

Les critères 3, 4 et 5 demandent un humain. Lance l'exe release, puis :

### Critère 3 : drop de fichiers et dossiers
1. Fais glisser un fichier de l'Explorateur sur la fenêtre principale.
   - Pendant le survol, le cadre du bas (zone de drop) devient rose.
   - Au lâcher, une ligne `[file] C:\...` apparaît dans le cadre.
2. Recommence avec un dossier : la ligne doit commencer par `[folder]`.
3. Dépose plusieurs éléments à la fois : une ligne par élément.
4. Vérifie que la fenêtre réagit toujours normalement après (redimensionner, cliquer).

### Critère 4 : touches média (SMTC)
1. Au démarrage, la ligne "Media keys (SMTC)" doit afficher `SMTC ready, press a media key`.
2. Appuie sur Lecture/Pause du clavier : le bouton du haut alterne Play/Pause et la ligne
   affiche `Toggle from MediaKey` (ou `Play` / `Pause`).
3. Appuie sur Suivant : "Track 1" devient "Track 2" et la ligne affiche `Next from MediaKey`.
4. Fenêtre au premier plan, puis au second plan (autre application active) : les touches
   doivent marcher dans les deux cas.
5. Ouvre le panneau média de Windows (touche volume) : le titre "Track N" et l'état doivent y
   apparaître.

### Critère 5 : icône de notification et mini-lecteur
1. Une pastille rose doit apparaître dans la zone de notification (clique sur la flèche ^ si
   elle est masquée).
2. Clic droit : menu Play / Pause, Next, Show main window, Show mini player, Quit.
3. Clic gauche sur l'icône : la fenêtre principale revient au premier plan.
4. "Show mini player" (ou le bouton "Mini player") ouvre une petite fenêtre toujours au-dessus.
5. Dans le mini-lecteur, clique Play puis Next : la fenêtre principale suit (même état). Fais
   l'inverse, puis depuis le menu de l'icône, puis avec les touches média : les deux fenêtres
   doivent toujours afficher le même état.
6. "Quit" du menu ferme tout, sans processus restant dans le Gestionnaire des tâches.

### Critères 1 et 2 : à confirmer à l'oeil (déjà mesurés par script)
1. Fais défiler la liste à la molette et à la barre : pas de saccade ni de ligne vide.
2. Tape dans le filtre (par ex. `night`) et clique sur les en-têtes Title / Artist / Album /
   Duration : le temps affiché en rose doit rester sous 100 ms.
3. Le spectrogramme défile sans à-coups ; la ligne au-dessus indique environ 60 Hz.
4. "Scroll benchmark" fait défiler 5 s et affiche les images par seconde rendues.
