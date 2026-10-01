# Transport disque direct et restitution de la RAM partagée

Date : 2026-10-01.

Statut : candidat implémenté, qualification de l'image finale en cours.

## Problème et décision

Sur le serveur Linux 6.8, les écritures du disque passant par FUSE sont
sérialisées avant d'atteindre le journal. Le regroupement durable seul ne réduit
donc pas le nombre de synchronisations. Le disque de travail utilise désormais
un socket vhost-user privé dans la jail, avec une seule virtqueue. Le disque
racine reste une image en lecture seule.

Le backend capture les métadonnées d'au plus 128 requêtes, puis copie les octets
des écritures dans des buffers possédés avant leur persistance : le contenu de
la RAM invitée peut changer concurremment. Les groupes sont limités à 8 Mio et
ne traversent ni une lecture ni une requête FLUSH. Aucune temporisation n'est
ajoutée pour remplir un groupe. Chaque confirmation positive attend la barrière
durable du journal ; Writeback conserve les requêtes FLUSH de l'invité.

Les adresses, directions, tailles et alignements des descripteurs sont vérifiés.
Les erreurs de persistance produisent une réponse disque en erreur. Une erreur
de protocole, un arrêt de la connexion ou un panic dans le callback deviennent
visibles au contrôleur, qui conserve le disque lors de l'arrêt de la VM. Le
socket est accessible uniquement à l'utilisateur de cette jail. La fermeture
arrête les workers et retire le socket, même avant la connexion du frontend.

## RAM partagée et correctif Firecracker

Le frontend crée un memfd partagé avec le backend. Firecracker 1.17.0 traite les
pages rendues par le balloon et free-page reporting avec MADV_DONTNEED, qui ne
libère pas leurs pages physiques dans cette configuration. Notre reproduction
libère une allocation de 512 Mio dans l'invité et gonfle le balloon : environ
819 Mio restent chargés au nœud jusqu'à l'arrêt de la VM.

L'image construit Firecracker depuis le commit amont
`95f868c8e345b1cc8faccd1a3c910b4989dc3f58`, archive vérifiée par SHA-256, avec
un correctif restreint à discard_range sur un fichier MAP_SHARED. Il vérifie
la plage complète et les conversions d'offset avant de supprimer uniquement
les pages abandonnées par l'invité avec PUNCH_HOLE | KEEP_SIZE. Les mappings
anonymes et les snapshots MAP_PRIVATE conservent leur traitement existant.
Le filtre seccomp amont permet déjà exactement ces flags ; il n'est pas élargi.

Le test de régression mesure les blocs physiques du memfd et vérifie les
octets de deux mappings, les régions voisines, les offsets des régions non
contiguës, la taille du fichier et le rejet des plages invalides. Il échoue sur
le code amont et passe avec le correctif. L'image exécute ce test et les tests
des mappings anonymes et privés avant de construire le binaire musl statique.
Le jailer reste le binaire amont 1.17.0 vérifié. Une mise à jour de Firecracker
doit réévaluer ce correctif et le retirer lorsque l'amont fournit la garantie.

La limite de taille des fichiers du jailer couvre le maximum entre disque et
RAM, puisque le memfd peut être plus grand que le disque de la conversation.
Le correctif et le backend doivent être distribués ensemble dans l'image du
nœud ; remplacer uniquement le binaire Leo sur une image ancienne ne suffit pas.

## Mesures du prototype et limites

Comparatif ABBA sur le serveur, VM 2 CPU/4 Gio, vrai Codex avec modèle local
simulé : les runs après échauffement passent de 11,9–13,5 s à 7,4–7,8 s. La
synchronisation finale passe de 1,4–2,0 s à 0,12–0,24 s. Les 759–796 écritures
nécessitent 195–206 barrières, soit environ 74 % de moins. Les premières
initialisations restent variables ; aucun gain de démarrage à froid garanti
ni gain en production n'est déduit de ce banc.

Le premier prototype passe 303,8 s de charge, 47 sauvegardes et 23 553 écritures
sans erreur disque ni redémarrage intempestif. La reprise dans une autre VM
conserve le contexte. Ce résultat précède le correctif mémoire et ne remplace
pas la qualification de l'image finale.

Avec le correctif mémoire, le même test sur une vraie VM restitue 513,5 Mio
pendant que la VM reste active : la charge memfd passe de 818,5 à 305,0 Mio.
Ces mesures utilisent des fixtures jetables et excluent le modèle externe et
S3. La qualification sous sauvegardes répétées, les tests Android et la mesure
des conversations réelles restent obligatoires avant livraison complète.
