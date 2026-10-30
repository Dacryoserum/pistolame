# Pistolame 0.8.0

- **Nouvelle page de lecture.** La page du morceau en cours devient un vrai lecteur : teintée par la couleur de la pochette, avec grande pochette, barre de lecture, commandes, et les morceaux précédent et suivant de chaque côté, cliquables. La file « À suivre » s'ouvre à côté. Les informations du fichier, le chemin du signal et l'authenticité sont juste en dessous.
- **Déplacement instantané.** Un clic dans la barre de lecture saute à la position voulue, sans pause ni reprise.
- **Glisser-déposer dans la file.** Attrapez les six points d'un morceau pour le monter ou le descendre.
- **Les plus écoutés.** Une playlist automatique classe vos morceaux. Choisissez sa taille et sa période (depuis toujours ou 30 derniers jours) dans Réglages. Le nombre d'écoutes s'affiche aussi sur la page de lecture et sur chaque album.
- **Bouton pause** à gauche du morceau en cours dans les listes.

### Corrections

- **Mode partagé (Windows) :** plus de son fort à chaque lecture, pause ou déplacement dans le morceau quand le volume est baissé.
- **Afficher dans l'Explorateur** ouvre le bon dossier, même si le chemin contient des espaces.
- **Fichiers illisibles :** un message clair remplace l'erreur technique en anglais.

# Changelog

## 0.8.0

### Features
- the current-track page is a full player tinted with the color of the cover, in the spirit of Apple's players: large sharp artwork that steps back while paused, title aligned under it, thin rail, bare controls, and the previous and next tracks either side, each clickable; "À suivre" opens beside it, and the file facts, signal path and authenticity sit below
- seeking while playing is near instant: the stream stays open, with no stop and no device reopen
- drag and drop in "À suivre": the six-dot handle moves a track within "Ensuite dans la file" or "Ensuite depuis"
- "Les plus écoutés": an automatic playlist ranking the tracks played most, on by default; Réglages sets its size and period (all time or the last 30 days), which the page can also switch
- play counts on the current-track page and on album pages
- a pause button left of the playing track in the track lists

### Fixes
- Windows shared mode: a stream reopened by a pause, a resume or a seek no longer starts at full level when the volume is lowered
- "Afficher dans l'Explorateur" opens the right folder when the path holds spaces
- a file with no recognizable audio content now gets a clear message instead of the decoder's raw error
