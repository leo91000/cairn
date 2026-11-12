# Disques de conversation locaux à leur node, sans S3

Date : 12 novembre 2026. Statut : accepté par Léo.

Cette décision remplace l’[ADR-0009](0009-s3-backed-disks-required-on-every-node.md)
et l’[ADR-0076](0076-direct-node-storage-transfers.md).

## Contexte

Depuis l’ADR-0009, le disque de travail d’une conversation est un journal local
durable dont les générations sont publiées sur S3. La reprise sur une autre node,
l’éviction du cache, la récupération après la perte d’une node et la migration entre
VM et natif lisent ces publications. Cela impose au manager un bucket et ses
identifiants, et à Cairn un pipeline de publication, des manifestes, des blocs
chiffrés et compressés partagés, leur collecte, des transferts directs et leur
qualification. S3 ne sert qu’aux disques de travail : l’historique, les pièces
jointes, les artefacts et leurs liens publics sont conservés par le manager sans
lui. Léo juge que cette complexité dépasse le bénéfice d’un déplacement sans perte.

## Décision

L’environnement de travail d’une conversation est purement local à sa node
d’exécution. Il n’en existe aucune copie distante : la perte ou la révocation d’une
node perd les fichiers de travail des conversations qui s’y trouvent. Leur historique,
leurs pièces jointes et leurs artefacts restent conservés.

Le disque garde son format : le journal local durable
([ADR-0007](0007-demand-paged-disks-with-durable-local-journal.md),
[ADR-0011](0011-segmented-local-journal.md)), monté par ublk dans la VM ou par le
contrôleur natif. Jusqu’ici, seul l’acquittement d’une publication S3 libérait le
journal. Cette consolidation devient locale : la node reconstruit une génération
scellée en blocs conservés sur son propre disque, les substitue à la base, puis
libère les segments correspondants. Ces blocs forment le disque lui-même et non un
cache : aucun budget ne les évince. La reprise sur la même node rouvre le journal et
ses blocs. Le manager ne publie, ne restaure et ne collecte plus aucun disque.

Changer de node revient à faire expirer l’environnement puis à démarrer un
environnement vide sur la destination, par le parcours existant de l’expiration
après inactivité ([rétention des conversations](../CONVERSATION-RETENTION.md)). La
conversation conserve son historique, ses pièces jointes et ses artefacts. La
session suivante reçoit l’historique visible, et l’agent est prévenu de la perte
des fichiers et de la date de l’expiration. Un changement confirmé interrompt le
tour en cours. Le web, Android, le MCP public et `cairn_installation` exigent une
confirmation explicite avec un texte du type : « Les fichiers de travail de cette
conversation seront perdus. L’historique, les pièces jointes et les artefacts sont
conservés. » Une demande sans confirmation est refusée. L’historique de la
conversation et l’avertissement donné à l’agent indiquent la cause de l’expiration :
inactivité, changement de node ou migration du stockage.

Une conversation attend ensuite son prochain message sur la destination ; une
mission active y reprend automatiquement. Un agent peut aussi changer sa propre
conversation de node avec `move_to_node`, à condition de confirmer explicitement
la perte de ses fichiers. Son tour s’arrête, l’environnement expire, puis la
conversation reprend automatiquement sur la destination dans un environnement vide,
avec le même avertissement.

Le placement automatique ([ADR-0073](0073-automatic-agent-placement.md)) choisit
toujours la node d’un nouvel environnement, mais ne déplace jamais une conversation
existante. La relocalisation faute de capacité et la reprise automatique sur une
autre node après une indisponibilité sont supprimées. Une conversation dont la node
est saturée, en maintenance, hors ligne ou révoquée attend cette node. Seul un
changement de node confirmé la fait repartir ailleurs, sans ses fichiers.

Une node n’exécute qu’un backend : faire passer une machine de VM à natif, ou
l’inverse, enrôle une seconde node avec son propre stockage
([exploitation des nodes natives](../NATIVE-NODES.md)). La migration d’une
conversation entre VM et exécution native est donc un changement de node et suit
le même parcours : confirmation, expiration, environnement vide sur le backend de
destination. Le montage natif d’un disque créé en VM
([ADR-0082](0082-move-vm-disks-between-execution-backends.md)) et la conversion
physique d’un disque créé en natif
([ADR-0054](0054-explicit-conversation-backend-migration.md)) n’ont plus de source
à lire ; ils sont retirés. `get_chat_backend_migration` et `migrate_chat_backend`
restent les points d’entrée de ce changement et exigent la même confirmation.

## Conversations existantes

Au déploiement, une migration idempotente fait expirer tout environnement dont le
disque dépend d’une publication S3, avec la cause « migration du stockage ». Aucun
bloc n’est rapatrié. Le travail en cours de ces conversations est interrompu ; leur
prochain message, ou la reprise d’une mission active, démarre un environnement vide.
Un environnement qui n’a jamais été publié est déjà entièrement local et reste
utilisable. Le nettoyage existant supprime les disques locaux expirés. Cairn n’efface
pas les objets distants : après la mise à jour, l’exploitant supprime le bucket, ses
identifiants et, le cas échéant, le Garage intégré. La note de version demande de
pousser avant la mise à jour les commits et les fichiers à conserver.

Le retour à une version antérieure exige la sauvegarde de la base prise avant la
mise à jour. Les fichiers de travail supprimés ne sont pas récupérables.

## Conséquences

Le manager n’a plus besoin de S3 ni d’identifiants AWS, et l’installateur n’embarque
plus Garage. Les réglages de stockage du web, d’Android, du MCP public et de
`cairn_installation` disparaissent, ainsi que les variables `STORAGE_S3_*`. Sont
retirés la lecture à la demande depuis S3, le codec et le chiffrement des blocs
distants, les manifestes, le pipeline de publication, les transferts directs, les
blocs partagés et leur collecte, l’éviction du cache propre et la restauration sur
une autre node.

La durabilité d’un environnement se limite désormais au disque de sa node. Une
sauvegarde de la machine reste à la charge de son exploitant. Une node conserve
chacun de ses environnements en entier : son espace n’est libéré que par
l’expiration ou la suppression d’une conversation. La réserve disque de la node et
la limite de stockage par tâche s’appliquent à ce volume complet ; une admission
sans espace attend.
