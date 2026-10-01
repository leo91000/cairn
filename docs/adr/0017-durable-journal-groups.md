# Partager les synchronisations du journal entre écritures en attente

Date : 2026-10-01.

Statut : prototype en validation, non déployé. Aucun gain de latence revendiqué.

## Problème

Le journal synchronise un segment pour chaque petite écriture. Lorsque le
transport permet plusieurs requêtes simultanées, elles attendent chacune une
barrière durable indépendante. La réservation de leur espace permet de relâcher
l'admission commune, mais ne supprime pas ces synchronisations.

## Décision

Écrire et indexer une trame complète sous le verrou du journal, puis attendre sa
durabilité sous un verrou distinct. Un demandeur capture le préfixe en attente,
synchronise tous ses fichiers hors du verrou du journal et marque uniquement ce
préfixe durable. Les écritures arrivées entre-temps restent en attente pour le
groupe suivant. Aucun délai artificiel, thread ni file supplémentaire n'est ajouté.

Une réponse positive exige que la séquence de l'écriture appartienne au préfixe
durable. Une erreur d'append ou de synchronisation invalide les confirmations
encore en attente et les prochaines écritures jusqu'à réouverture. Des trames
complètes mais non confirmées peuvent survivre à un crash ; la récupération les
vérifie et synchronise les segments avant de les intégrer au préfixe durable.

Les écritures conservent une garde de publication jusqu'à leur confirmation :
aucun fichier capturé par une synchronisation ne peut être supprimé pendant
celle-ci. Les barrières de synchronisation et de scellement prennent une garde
exclusive d'écriture, qui attend la fin de toutes les confirmations en cours.
Les descripteurs SQLite, le format des trames, les sommes de contrôle et les
reçus de publication restent inchangés.

Les lectures concurrentes peuvent observer une trame complète avant sa
confirmation, comme une lecture chevauchant une écriture normale. Une sauvegarde
ne peut pas publier cette trame avant le scellement durable.

## Ordre des verrous

Écriture : garde d'écriture partagée, garde de publication partagée, journal
pour staging et comptabilité, puis verrou de commit sans conserver le journal.
Commit : verrou de commit, journal pour capture, I/O hors journal, journal pour
confirmation. Scellement : garde d'écriture exclusive puis journal et SQLite.
Publication : garde de publication exclusive puis journal et SQLite. Aucun
appel ne doit attendre le verrou de commit en conservant celui du journal.

## Validation et limites

Les tests déterministes couvrent quatre demandeurs pour une seule barrière,
l'absence de confirmation précoce, les erreurs propagées, les barrières de
scellement et synchronisation, les ajouts après capture et la rotation des
segments. Les tests tuent également le processus avant et après la synchronisation
du groupe, puis après sa confirmation, et vérifient les écritures confirmées
après réouverture.

Le transport synchrone actuel peut n'offrir aucun regroupement dans une VM.
Les options FUSE et Firecracker asynchrones restent expérimentales : mesurer
leurs effets et qualifier leur compatibilité est nécessaire avant livraison.
Le banc réel, les sauvegardes répétées et la validation de l'image exacte restent
à effectuer. Les compteurs de barrières et de trames permettent de distinguer un
regroupement effectif d'une simple concurrence des callbacks.

## Premières mesures

Sur ext4 local, quatre demandeurs écrivant chacun 128 trames de 4 Kio donnent
1,86–4,53 s avec le protocole antérieur, contre 0,91–1,16 s avec les groupes.
Les 512 barrières deviennent 138–196. Les données sont vérifiées après
réouverture. Ce petit banc exclut FUSE, la VM, le modèle et S3 ; il n'est pas une
mesure du démarrage de l'agent.

Sur le poste, le banc Firecracker avec transport asynchrone observe des groupes
de trois à quatre trames et environ 37 % de barrières en moins. La variation de
latence entre les références du comparatif empêche d'attribuer un gain global au
regroupement seul.

Sur le serveur Linux 6.8, le même prototype reçoit une seule écriture à la fois :
le journal ne regroupe rien. La configuration capturée indique bien Async ; le
descripteur du disque n'a pas O_DIRECT. Dans le code Linux 6.8, le chemin FUSE
asynchrone dépend également de IOCB_DIRECT, et les écritures io_uring déportées
sont sérialisées par inode en l'absence de O_DIRECT. Le code Linux récent permet
le chemin FUSE asynchrone sans cette condition. Les sources pertinentes sont
[fuse/file.c en 6.8](https://github.com/torvalds/linux/blob/v6.8/fs/fuse/file.c),
[io_uring.c en 6.8](https://github.com/torvalds/linux/blob/v6.8/io_uring/io_uring.c)
et [fuse/file.c récent](https://github.com/torvalds/linux/blob/master/fs/fuse/file.c).

Les observations correspondent à cette différence de transport. Un changement
qui élimine réellement la sérialisation sur le serveur doit encore être testé
avant d'attribuer un gain causal et de retenir cette architecture en production.
