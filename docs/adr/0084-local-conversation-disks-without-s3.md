# Disques de conversation locaux à leur node, sans S3

Date : 12 novembre 2026. Statut : accepté par Léo.

Cette décision remplace l’[ADR-0076](0076-direct-node-storage-transfers.md) et
l’[ADR-0009](0009-s3-backed-disks-required-on-every-node.md) (sauf le journal local
comme format unique du disque), ainsi que la conservation de
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
Révoquer une node fait donc expirer les environnements qu’elle contient ; une
conversation dont la node est simplement perdue expire lors de sa révocation.

Le disque garde son format : le journal local durable
([ADR-0007](0007-demand-paged-disks-with-durable-local-journal.md),
[ADR-0011](0011-segmented-local-journal.md)), exposé par ublk à la VM ou au
contrôleur natif. Jusqu’ici, seul l’acquittement d’une publication S3 libérait les
segments du journal. La node consolide désormais elle-même son journal, afin que
son volume reste borné par celui du disque. Aucune donnée d’un disque n’est retirée
de sa node tant que l’environnement existe : la reprise sur la même node rouvre le
journal. Arrêter une VM retenue conserve son disque sur la node, et aucune
publication ne conditionne plus cet arrêt. Le manager ne publie, ne restaure et ne
collecte plus aucun disque.

### Changement de node

Changer de node revient à faire expirer l’environnement puis à démarrer un
environnement vide sur la destination, comme après une expiration pour inactivité
([rétention des conversations](../CONVERSATION-RETENTION.md)). La conversation
conserve son historique, ses pièces jointes et ses artefacts. La session suivante
reçoit l’historique visible, et l’agent est prévenu de la perte des fichiers et de
la date de l’expiration. L’historique de la conversation et cet avertissement
indiquent la cause : inactivité, changement de node, révocation de la node ou
migration du stockage.

Quelle que soit la cause, inactivité comprise, l’expiration retire immédiatement
l’ancien environnement de la conversation : l’état bloquant de nettoyage disparaît.
La suppression de son disque se poursuit en arrière-plan sur sa node et ne bloque ni
les messages ni un changement de node : la node le supprime quand elle est
joignable, et il est oublié si elle est révoquée. Une node supprime l’ancien disque
d’une conversation avant d’admettre son nouvel environnement. La suppression d’une
conversation garde le comportement de
l’[ADR-0041](0041-immediate-conversation-deletion.md).

Avant ce changement, le web et Android affichent, et le MCP public et
`cairn_installation` renvoient, un avertissement du type : « Les fichiers de travail de cette conversation seront
perdus. L’historique, les pièces jointes et les artefacts sont conservés. » Toute
opération qui change la node d’un environnement existant exige une confirmation
explicite et refuse une demande qui n’en contient pas : l’opération `move_to_node`
du MCP public et de `cairn_installation`, utilisée aussi par le web et Android,
l’outil `move_to_node` de l’espace de travail d’un agent et `migrate_chat_backend`.
`get_chat_backend_migration` reste un aperçu en lecture seule qui présente cet
avertissement. Un environnement déjà expiré n’a plus de fichiers à perdre : choisir
sa prochaine node ne demande pas cet avertissement.

La destination et les autorisations sont vérifiées avant l’expiration, qui est
irréversible. Le changement interrompt alors le tour en cours et clôt les questions
ouvertes ; la destination est fixée pour le nouvel environnement. Sauf lorsque
l’agent change lui-même de node, une conversation attend ensuite son prochain
message ; les messages en file et les attentes planifiées démarrent sur la
destination, et une mission active y reprend automatiquement.

Un agent peut aussi changer sa propre conversation de node avec l’outil
`move_to_node` de son espace de travail, à condition de confirmer explicitement la
perte de ses fichiers. Son tour s’arrête, l’environnement expire, puis la
conversation reprend automatiquement sur la destination dans un environnement vide,
avec le même avertissement.

Le placement automatique ([ADR-0073](0073-automatic-agent-placement.md)) choisit
toujours la node d’un nouvel environnement, mais ne change jamais la node d’un
environnement existant. La relocalisation faute de capacité et la reprise
automatique sur une autre node après une indisponibilité sont supprimées. Une conversation dont la node
est saturée, en maintenance ou hors ligne attend cette node. Une conversation dont
l’agent n’a plus accès à la node de son environnement reste arrêtée jusqu’au retour
de cet accès ou à un changement de node confirmé. Après une révocation, le prochain
tour démarre un environnement vide par le placement automatique ; libérer les choix
de node (`releasePlacements`) garde son rôle pour les préférences et fixations.

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
bloc n’est rapatrié. Le travail en cours de ces conversations est interrompu ; leur
prochain message, ou la reprise d’une mission active, démarre un environnement vide.
Un environnement qui n’a jamais été publié est déjà entièrement local et reste
utilisable.

Cairn n’efface plus aucun objet distant, y compris les suppressions encore en
attente : après la mise à jour, supprimer une conversation ne retire plus ses
anciens blocs du bucket. La note de version demande de pousser les commits et de
sauvegarder les fichiers à conserver avant la mise à jour. Les fichiers expirés ne
reviennent qu’avec un retour complet à la base de données sauvegardée avant la mise
à jour et au bucket correspondant, et seulement jusqu’à leur dernière génération
publiée. La mise à jour retire le service Garage intégré sans effacer ses données.
L’exploitant conserve donc le bucket, ses identifiants et ces données tant qu’un
retour arrière reste envisagé, puis les supprime.

## Conséquences

Le manager n’a plus besoin de S3 ni de ses identifiants, et l’installateur n’embarque
plus Garage. Les réglages S3 du web, d’Android, du MCP public et de
`cairn_installation` disparaissent, ainsi que les variables `STORAGE_S3_*`, le
budget de cache de publication de l’installation et, pour les nodes, l’intervalle de
synchronisation, le délai de retard de synchronisation et le budget de cache propre.
Restent la réserve disque et le cache mémoire de chaque node, les délais de
déconnexion, d’arrêt et d’attente de capacité de l’installation, et la limite de
stockage par tâche. Sont retirés la lecture à la
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
