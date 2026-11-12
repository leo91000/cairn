# Disques de conversation locaux à leur node, sans S3

Date : 12 novembre 2026. Statut : accepté par Léo.

Cette décision remplace l’[ADR-0009](0009-s3-backed-disks-required-on-every-node.md)
et l’[ADR-0076](0076-direct-node-storage-transfers.md), ainsi que la conservation de
l’environnement lors d’un déplacement
([ADR-0002](0002-conversation-movement-by-pause-and-resume.md)), la publication des
disques ([ADR-0008](0008-current-disk-publication-without-archives.md),
[ADR-0010](0010-shared-block-incarnations.md),
[ADR-0021](0021-final-publication-without-blocking-the-next-turn.md),
[ADR-0023](0023-periodic-journal-capture-without-guest-freeze.md),
[ADR-0027](0027-stopped-disk-publication-reader.md),
[ADR-0036](0036-compressed-immutable-disk-objects.md)) et la réutilisation des
disques entre backends ([ADR-0054](0054-explicit-conversation-backend-migration.md),
[ADR-0082](0082-move-vm-disks-between-execution-backends.md)). Les statuts des
ADR partiellement concernées indiquent ce qui reste applicable.

## Contexte

Depuis l’ADR-0009, le disque de travail d’une conversation est un journal local
durable dont les générations sont publiées sur S3. La reprise sur une autre node,
l’éviction du cache, la récupération après la perte d’une node et la migration entre
VM et natif reposent sur ces publications. Cela impose au manager un bucket et ses
identifiants, et à Cairn un pipeline de publication, des manifestes, des blocs
chiffrés et compressés partagés, leur collecte, des transferts directs et leur
qualification. S3 ne sert qu’aux disques de travail : l’historique, les pièces
jointes, les artefacts et leurs liens publics sont conservés par le manager sans
lui. Léo juge que cette complexité dépasse le bénéfice d’un déplacement sans perte.

## Décision

L’environnement de travail d’une conversation est purement local à sa node
d’exécution. Il n’en existe aucune copie distante : la perte ou la révocation d’une
node entraîne la perte des fichiers de travail des conversations qui s’y trouvent.
Leur historique, leurs pièces jointes et leurs artefacts restent conservés.

Le disque garde son format : le journal local durable
([ADR-0007](0007-demand-paged-disks-with-durable-local-journal.md),
[ADR-0011](0011-segmented-local-journal.md)), monté par ublk dans la VM ou par le
contrôleur natif. Jusqu’ici, seul l’acquittement d’une publication S3 libérait les
segments du journal. La node consolide désormais elle-même son journal, afin que
son volume reste borné par celui du disque. Aucune donnée d’un disque n’est évincée :
la reprise sur la même node rouvre le journal. Arrêter une VM retenue conserve son
disque sur la node ; aucune publication n’en conditionne l’éviction. Le manager ne
publie, ne restaure et ne collecte plus aucun disque.

### Changement de node

Changer de node revient à faire expirer l’environnement puis à démarrer un
environnement vide sur la destination, comme après une expiration pour inactivité
([rétention des conversations](../CONVERSATION-RETENTION.md)). La conversation
conserve son historique, ses pièces jointes et ses artefacts. La session suivante
reçoit l’historique visible, et l’agent est prévenu de la perte des fichiers et de
la date de l’expiration. L’historique de la conversation et cet avertissement
indiquent la cause : inactivité, changement de node ou migration du stockage.

Le web, Android, le MCP public et `cairn_installation` affichent avant ce changement
un avertissement du type : « Les fichiers de travail de cette conversation seront
perdus. L’historique, les pièces jointes et les artefacts sont conservés. » Toute
opération qui change la node d’une conversation existante, dont `move_to_node` et
`migrate_chat_backend`, exige une confirmation explicite et refuse une demande qui
n’en contient pas ; `get_chat_backend_migration` reste un aperçu en lecture seule
qui présente cet avertissement.

La destination et les autorisations sont vérifiées avant l’expiration, qui est
irréversible. Le changement interrompt alors le tour en cours et clôt les questions
ouvertes ; la destination est fixée pour le nouvel environnement. Une conversation
attend ensuite son prochain message ; les messages en file et les attentes
planifiées démarrent sur la destination, et une mission active y reprend
automatiquement. Le changement n’attend pas que la node d’origine supprime l’ancien
disque : celui-ci n’appartient plus à la conversation. La node d’origine le supprime
quand elle redevient joignable ; il est oublié si elle est révoquée.

Un agent peut aussi changer sa propre conversation de node avec l’outil
`move_to_node` de son espace de travail, à condition de confirmer explicitement la
perte de ses fichiers. Son tour s’arrête, l’environnement expire, puis la
conversation reprend automatiquement sur la destination dans un environnement vide,
avec le même avertissement.

Le placement automatique ([ADR-0073](0073-automatic-agent-placement.md)) choisit
toujours la node d’un nouvel environnement, mais ne déplace jamais une conversation
existante. La relocalisation faute de capacité et la reprise automatique sur une
autre node après une indisponibilité sont supprimées. Une conversation dont la node
est saturée, en maintenance ou hors ligne attend cette node. Une conversation dont la
node est révoquée, ou dont l’agent n’y a plus accès, reste arrêtée jusqu’à un
changement de node confirmé.

Une node n’exécute qu’un backend : faire passer une machine de VM à natif, ou
l’inverse, enrôle une seconde node avec son propre stockage
([exploitation des nodes natives](../NATIVE-NODES.md)). La migration d’une
conversation entre VM et exécution native est donc un changement de node : même
confirmation, même expiration, environnement vide sur le backend de destination.
Ses vérifications d’autorisation et d’autorité native restent celles des
ADR-0054 et ADR-0082. Le montage natif d’un disque créé en VM et la conversion
physique d’un disque créé en natif n’ont plus de source à lire ; ils sont retirés.

## Conversations existantes

Au déploiement, une migration idempotente fait expirer tout environnement dont le
disque dépend d’une publication S3, avec la cause « migration du stockage ». Aucun
bloc n’est rapatrié. Comme pour un changement de node, l’ancien environnement est
retiré immédiatement de la conversation, et la suppression des disques locaux se
poursuit en arrière-plan sans bloquer les messages. Le travail en cours de ces
conversations est interrompu ; leur prochain message, ou la reprise d’une mission
active, démarre un environnement vide. Un environnement qui n’a jamais été publié
est déjà entièrement local et reste utilisable.

Cairn n’efface pas les objets distants. La note de version demande de pousser les
commits et de sauvegarder les fichiers à conserver avant la mise à jour. Le retour
à une version antérieure exige la sauvegarde de la base prise avant la mise à jour
et les objets correspondants du bucket : l’exploitant conserve donc le bucket et ses
identifiants tant qu’un retour arrière reste envisagé, puis les supprime avec, le
cas échéant, le Garage intégré. Les fichiers de travail expirés ne sont plus
récupérables une fois le bucket supprimé.

## Conséquences

Le manager n’a plus besoin de S3 ni de ses identifiants, et l’installateur n’embarque
plus Garage. Les réglages S3 du web, d’Android, du MCP public et de
`cairn_installation` disparaissent, ainsi que les variables `STORAGE_S3_*` et les
réglages de synchronisation et de cache propre des nodes. La réserve disque d’une
node et la limite de stockage par tâche restent. Sont retirés la lecture à la
demande depuis S3, le codec et le chiffrement des blocs distants, les manifestes
publiés, le pipeline de publication, les transferts directs, les blocs partagés et
leur collecte, l’éviction du cache propre, la restauration sur une autre node, ainsi
que leurs tests et leur documentation.

La durabilité d’un environnement se limite désormais au disque de sa node. Une
sauvegarde de la machine reste à la charge de son exploitant. Une node conserve
chacun de ses environnements en entier : son espace n’est libéré que par
l’expiration ou la suppression d’une conversation, ou par un changement de node.
Si l’expiration automatique est désactivée, rien ne libère l’espace d’une node
pleine sans action explicite. La réserve disque de la node et la limite de stockage
par tâche s’appliquent à ce volume complet ; une admission sans espace attend.
