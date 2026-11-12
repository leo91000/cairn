# Disques de conversation locaux à leur node, sans S3

Date : 12 novembre 2026. Statut : accepté par Léo.

Cette décision remplace l’[ADR-0076](0076-direct-node-storage-transfers.md) et
l’[ADR-0009](0009-s3-backed-disks-required-on-every-node.md) (sauf le journal local
comme format unique du disque), ainsi que la conservation de l’environnement lors
d’un déplacement ([ADR-0002](0002-conversation-movement-by-pause-and-resume.md)), la
publication des disques ([ADR-0008](0008-current-disk-publication-without-archives.md),
[ADR-0010](0010-shared-block-incarnations.md),
[ADR-0021](0021-final-publication-without-blocking-the-next-turn.md),
[ADR-0023](0023-periodic-journal-capture-without-guest-freeze.md),
[ADR-0027](0027-stopped-disk-publication-reader.md),
[ADR-0036](0036-compressed-immutable-disk-objects.md)) et la conversion et la
réutilisation des disques entre backends
([ADR-0054](0054-explicit-conversation-backend-migration.md),
[ADR-0082](0082-move-vm-disks-between-execution-backends.md)). Les statuts des ADR
partiellement concernées indiquent ce qui reste applicable.

## Contexte

Depuis l’[ADR-0009](0009-s3-backed-disks-required-on-every-node.md), le disque de
travail d’une conversation est un journal local durable dont les générations sont
publiées sur S3. La reprise sur une autre node, l’éviction du cache, la récupération
après la perte d’une node et la migration entre VM et natif reposent sur ces
publications. Cela impose au manager un bucket et ses identifiants, et à Cairn un
pipeline de publication, des manifestes, des blocs chiffrés et compressés partagés,
leur collecte, des transferts directs et leur qualification. S3 ne sert qu’aux
disques de travail : l’historique, les pièces jointes, les artefacts et leurs liens
publics sont conservés par le manager sans lui. Léo juge que cette complexité
dépasse le bénéfice d’un déplacement sans perte.

## Décision

L’environnement de travail d’une conversation est purement local à sa node
d’exécution. Il n’en existe aucune copie distante : la perte ou la révocation d’une
node entraîne la perte des fichiers de travail des conversations qui s’y trouvent.
Leur historique, leurs pièces jointes et leurs artefacts restent conservés.

Le disque garde son format : le journal local durable
([ADR-0007](0007-demand-paged-disks-with-durable-local-journal.md),
[ADR-0011](0011-segmented-local-journal.md)), exposé par ublk à la VM ou au
contrôleur natif. Jusqu’ici, seul l’acquittement d’une publication S3 libérait les
segments du journal. La node consolide désormais elle-même le journal de chaque
disque, afin que son volume reste borné par celui du disque. Aucune donnée d’un
disque n’est retirée de sa node tant que l’environnement existe : la reprise sur la
même node rouvre le journal. Arrêter une VM retenue conserve son disque sur la node,
et aucune publication ne conditionne plus cet arrêt. Le manager ne publie, ne
restaure et ne collecte plus aucun disque.

### Expiration de l’environnement

Changer de node revient à faire expirer l’environnement puis à démarrer un
environnement vide sur la destination, comme après une expiration pour inactivité
([rétention des conversations](../CONVERSATION-RETENTION.md)). La conversation
conserve son historique, ses pièces jointes et ses artefacts. La session suivante
reçoit l’historique visible, et l’agent est prévenu de la perte des fichiers et de
la date de l’expiration. L’historique de la conversation et cet avertissement
indiquent la cause : inactivité, changement de node, révocation de la node ou
migration du stockage.

Quelle que soit la cause, inactivité comprise, l’expiration retire immédiatement
l’ancien environnement de la conversation : l’état bloquant de nettoyage disparaît,
y compris pour les environnements qui s’y trouvent déjà. La suppression de l’ancien
disque se poursuit en arrière-plan sur sa node et ne bloque ni les messages ni un
changement de node : la node le supprime quand elle est joignable, et il est oublié
si elle est révoquée. Une node supprime l’ancien disque d’une conversation avant
d’admettre son nouvel environnement. Un nouvel environnement n’est admis qu’après
l’arrêt confirmé de l’exécution précédente de la conversation, ou l’échéance de son
bail si sa node est injoignable. La suppression d’une conversation garde le
comportement de l’[ADR-0041](0041-immediate-conversation-deletion.md), sans effacer
d’objet distant ; un disque resté sur une node révoquée est oublié.

Un nouvel environnement garde le backend enregistré de la conversation. Pour un
environnement déjà expiré, qui n’a plus de fichiers à perdre, choisir la prochaine
node ou le prochain backend passe par les choix de placement existants ou par
`migrate_chat_backend`, sans avertissement.

### Changement de node confirmé

Avant un changement de node, le web et Android affichent, et le MCP public et
`cairn_installation` renvoient, un avertissement du type : « Les fichiers de travail
de cette conversation seront perdus. L’historique, les pièces jointes et les
artefacts sont conservés. » La confirmation de cette perte est un paramètre nouveau
et distinct, exigé par toute opération qui change la node d’un environnement
existant : l’opération `move_to_node` du MCP public et de `cairn_installation`,
utilisée aussi par le web et Android, l’outil `move_to_node` de l’espace de travail
d’un agent et `migrate_chat_backend`. Le `confirm: true` existant de
`migrate_chat_backend` ne suffit plus. Une demande sans la nouvelle confirmation est
refusée avec cet avertissement. `get_chat_backend_migration` reste un aperçu en
lecture seule qui le présente.

La destination et les autorisations sont vérifiées avant l’expiration, qui est
irréversible. Le changement interrompt ensuite le tour en cours et clôt les
questions ouvertes. La première admission du nouvel environnement est imposée sur la
destination et attend sa capacité tant que celle-ci reste autorisée et non révoquée,
puis revient au placement automatique ; la conversation n’est pas fixée à cette
node. Sauf lorsque l’agent change lui-même la node de sa propre conversation,
celle-ci attend ensuite son prochain message ; les messages en file et les attentes
planifiées démarrent sur la destination, et une mission active y reprend
automatiquement.

Un agent peut aussi changer sa propre conversation de node, avec l’outil
`move_to_node` de son espace de travail ou avec `cairn_installation`, à condition de
confirmer explicitement la perte de ses fichiers. Son tour s’arrête, l’environnement
expire, puis la conversation reprend automatiquement sur la destination dans un
environnement vide, avec l’avertissement d’expiration décrit plus haut.

Une node n’exécute qu’un backend : faire passer une machine de VM à natif, ou
l’inverse, enrôle une seconde node avec son propre stockage
([exploitation des nodes natives](../NATIVE-NODES.md)). La migration d’une
conversation entre VM et exécution native est donc un changement de node : même
confirmation, même expiration, environnement vide sur le backend de destination. Ses
vérifications d’autorisation et d’autorité native restent celles des
[ADR-0054](0054-explicit-conversation-backend-migration.md) et
[ADR-0082](0082-move-vm-disks-between-execution-backends.md). Le montage natif d’un
disque créé en VM et la conversion physique d’un disque créé en natif n’ont plus de
source à lire ; ils sont retirés.

### Placement et révocation

Le placement automatique ([ADR-0073](0073-automatic-agent-placement.md)) continue de
choisir la node d’un nouvel environnement lorsqu’aucune destination n’est imposée,
mais ne change jamais la node d’un environnement existant. La relocalisation faute
de capacité et la reprise automatique sur une autre node après une indisponibilité
sont supprimées. Une conversation dont la node est saturée, en maintenance ou hors
ligne attend cette node. Une conversation dont l’agent n’a plus accès à la node de
son environnement reste arrêtée jusqu’au retour de cet accès ou à un changement de
node confirmé.

Révoquer une node fait expirer les environnements qu’elle contient ; l’environnement
d’une conversation dont la node est simplement perdue expire lors de la révocation
de cette node. Sur le web, Android, le MCP public et `cairn_installation`, la
révocation avertit de la perte des fichiers de travail et exige la même confirmation
distincte ; `revoke_node` refuse une demande qui n’en contient pas. Le prochain tour
de ces conversations démarre un environnement vide par le placement automatique,
sauf pour une conversation fixée à cette node lorsque la révocation n’a pas libéré
les placements (`releasePlacements`) : elle attend que ce choix soit retiré ou
remplacé.

## Conversations existantes

Au déploiement, une migration idempotente fait expirer, avec la cause « migration du
stockage », tout environnement dont le disque dépend d’une publication S3 ou qui se
trouve sur une node déjà révoquée. Aucun bloc n’est rapatrié. Le travail en cours de
ces conversations est interrompu ; leur prochain message, ou la reprise d’une
mission active, démarre un environnement vide. Un environnement qui n’a jamais été
publié et dont la node est active est déjà entièrement local et reste utilisable. La
migration annule aussi les déplacements, relocalisations, reprises et migrations de
backend en attente, sans les rejouer ni en conserver la destination.

Cairn n’efface plus aucun objet distant, y compris les suppressions encore en
attente : après la mise à jour, supprimer une conversation ne retire plus ses
anciens blocs du bucket. Les installations mises à jour automatiquement perdent ces
fichiers de travail à la mise à jour, sans autre préavis que la note de version, qui
demande de pousser les commits et de sauvegarder les fichiers à conserver. Le retour
à une version antérieure exige une sauvegarde de la base de données faite par
l’exploitant avant la mise à jour, que le superviseur ne fait pas, et le bucket
correspondant ; les fichiers ne reviennent que jusqu’à leur dernière génération
publiée. La mise à jour n’efface ni `storage-s3.json`, ni les valeurs
`STORAGE_S3_*` de l’environnement, ni la configuration et les données de Garage. Les
nouvelles installations n’embarquent plus Garage ; le superviseur déjà installé ne
retire pas ce service, que l’exploitant arrête et retire, avec le bucket et ses
identifiants, quand aucun retour arrière n’est envisagé.

## Conséquences

Le manager n’a plus besoin de S3 ni de ses identifiants. Les réglages S3 du web,
d’Android, du MCP public et de `cairn_installation` disparaissent, ainsi que les
variables `STORAGE_S3_*`, l’objectif de synchronisation et le budget de cache de
publication de l’installation et, pour les nodes, l’intervalle de synchronisation,
le délai de retard de synchronisation et le budget de cache propre. Restent la
réserve disque et le cache mémoire de chaque node, les délais de déconnexion,
d’arrêt et d’attente de capacité de l’installation, et la limite de stockage par
tâche. Sont retirés la lecture à la demande depuis S3, le codec et le chiffrement
des blocs distants, les manifestes publiés, le pipeline de publication, les
transferts directs, les blocs partagés et leur collecte, l’éviction du cache propre,
la restauration sur une autre node, ainsi que leurs tests et leur documentation.

La durabilité d’un environnement se limite désormais au disque de sa node. Une
sauvegarde de la machine reste à la charge de son exploitant. Aucun environnement
n’est évincé : seules la consolidation de son journal, l’expiration de
l’environnement et la suppression de la conversation libèrent de l’espace. Si
l’expiration automatique est désactivée, rien ne libère l’espace d’une node pleine
sans action explicite. La réserve disque de la node et la limite de stockage par
tâche s’appliquent à ce volume complet ; une admission sans espace attend.

Les guides d’exploitation, le code et les interfaces sont mis à jour par les
changements qui appliquent cette décision.
