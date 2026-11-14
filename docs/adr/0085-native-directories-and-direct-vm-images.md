# Répertoires natifs et images VM locales sans transport applicatif

Décision demandée par Léo le 14 novembre 2026. Cette décision remplace la partie
format/transport de l’[ADR-0083](0083-local-conversation-disks-without-s3.md), les
journaux et les bases préparées de l’[ADR-0052](0052-native-ublk-workspaces-and-idle-eviction.md),
ainsi que l’obligation ublk de l’[ADR-0055](0055-ublk-only-vm-storage.md).
L’environnement reste local à sa node. L’expiration configurable, les confirmations
de changement de node et de révocation, la corbeille, l’historique et les artefacts
conservent leurs contrats. La suppression de S3 ne désactive pas la rétention.

Une exécution native utilise un répertoire privé retenu pour ses fichiers, son
home, ses données Docker et son temporaire géré. Le contrôleur conserve ses
métadonnées de contrôle hors du quota de ce répertoire : un environnement plein
doit encore pouvoir reprendre, appliquer une capacité accordée et libérer ses
fichiers. La première implémentation vise un système de fichiers ext4 dédié, avec
quotas de projet effectivement appliqués. Les limites de blocs et d’inodes sont
imposées par le noyau ; les cgroups ne bornent pas l’espace occupé. La comptabilité
partagée utilise les octets alloués et les crédits d’appartenance vérifiés, sans
facturer les trous d’un fichier sparse comme des blocs physiques.

La VM reçoit directement un fichier ext4 local sur son interface bloc Firecracker.
Sa taille borne sa capacité. L’image est retenue après arrêt, et la même node la
rouvre au tour suivant. Aucun journal applicatif, transport ublk, copie distante ou
consolidation périodique ne s’interpose entre ces nouveaux environnements et leur
système de fichiers. Les caches de projets natifs sont des arbres de fichiers
vérifiés ; leur copie privée n’emprunte pas l’autorité d’une autre conversation.

Les limites CPU, RAM et processus restent des cgroups systemd. Le contrôleur exige
aussi le contrôleur IO et applique une limite de lecture et d’écriture de 64 MiB/s
par exécution sur le système de fichiers du travail, sous un plafond partagé de
256 MiB/s. Ces plafonds sont distincts de la priorité relative IO et du quota de
stockage. Leur qualification doit vérifier les valeurs réellement appliquées.

Arrêter une exécution ferme ses processus, ses conteneurs attribués et ses
ressources temporaires, tout en retenant les fichiers nécessaires à une reprise.
Le cgroup principal vide ne suffit pas à prouver l’arrêt de consommateurs gérés
par un autre service, notamment le Docker administrateur. L’appartenance doit
être conservée et revérifiée avant toute action ; un PID ou numéro de périphérique
historique ne constitue jamais cette preuve. Une clôture incertaine conserve
l’ownership et une erreur observable, avec reprise du nettoyage.

L’expiration ou la suppression de l’environnement efface ensuite ses répertoires,
images, volumes et données Docker privées ainsi que les restes techniques
attribués. La file durable et ses reçus de retraite restent idempotents après crash,
redémarrage ou retour d’une node hors ligne. Un reçu d’un ancien environnement ne
peut pas effacer le suivant. Les petits reçus d’idempotence et les inodes de verrou
restent volontairement conservés : les retirer rendrait possibles une répétition
destructrice ou deux propriétaires concurrents. Les caches partagés de projets
restent soumis à leur budget propre ; ils n’appartiennent pas au travail privé
d’une conversation.

Les journaux locaux existants ne sont pas déclarés vides. Leur conversion garde
la source jusqu’à la vérification complète et à la publication durable du format
remplaçant. Pour une VM, les octets exportés sont comparés à la source avant
vérification ext4 et bascule vers l’image. Pour le natif, le chemin de compatibilité
sert seulement à copier et vérifier les fichiers retenus avant de retirer l’ancien
transport. Toute phase interrompue doit être rejouable ; les anciens fichiers ne
sont effacés qu’après la bascule durable. Un format inconnu ou une source manquante
est refusé explicitement, sans initialiser silencieusement un environnement vide.
Le retour à l’ancien format exige une sauvegarde ; aucun déploiement de cette
migration en production n’est autorisé par cette implémentation.

Le merge exige les qualifications natives et VM sur infrastructure jetable,
plusieurs cycles création/écriture/arrêt/reprise/expiration, les cas quota et
ENOSPC, arrêt forcé, crash, consommateurs Docker survivants et nettoyage retardé.
Les mesures comparent une référence exacte aux nouveaux chemins : octets logiques
et alloués, lectures froides/chaudes, écritures et CPU, avec commandes et conditions
de l’hôte. Elles restent locales, sans inférence sur une latence réseau de production.
