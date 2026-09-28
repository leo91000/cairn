# Disques à la demande et journal local durable

> [ADR-0009](0009-s3-backed-disks-required-on-every-node.md) rend ce mode obligatoire sur toutes les nodes et retire le chemin dm-era local.

La reprise avant téléchargement complet impose de suivre exactement les octets locaux et distants : le nouvel adapter de disque utilise un manifeste distant immuable et un journal local durable qui intercepte les écritures, tandis que `dm-era` reste utilisé pour les disques entièrement locaux décrits dans l'ADR-0005.
Nous choisissons un premier adapter FUSE en E/S directes, sous condition d'une validation avec le vrai Firecracker et son jailer ; les synchronisations invitées doivent atteindre le journal persistant et aucune donnée non sauvegardée ne peut être évincée.
La publication distante reste asynchrone conformément à l'ADR-0004, avec pauses annulables et reprise automatique lors d'une panne S3, d'un manque d'espace ou d'un retard de protection supérieur au seuil configuré (5 minutes par défaut).

Lors d'une pause imposée par la réserve disque, attendre le gel du système de
fichiers peut créer une dépendance circulaire avec une écriture en attente de
place. Une capture d'urgence du journal durable, CPU en pause, est alors marquée
« crash » et nécessite la récupération ext4 habituelle après arrêt brutal. Les
captures normales conservent le gel du système de fichiers. Cette exception
n'améliore pas la garantie de cohérence applicative de l'ADR-0004.
