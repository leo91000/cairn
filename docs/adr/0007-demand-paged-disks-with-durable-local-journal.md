# Disques à la demande et journal local durable

La reprise avant téléchargement complet impose de suivre exactement les octets locaux et distants : le nouvel adapter de disque utilise un manifeste distant immuable et un journal local durable qui intercepte les écritures, tandis que `dm-era` reste utilisé pour les disques entièrement locaux décrits dans l'ADR-0005.
Nous choisissons un premier adapter FUSE en E/S directes, sous condition d'une validation avec le vrai Firecracker et son jailer ; les synchronisations invitées doivent atteindre le journal persistant et aucune donnée non sauvegardée ne peut être évincée.
La publication distante reste asynchrone conformément à l'ADR-0004, avec pauses annulables et reprise automatique lors d'une panne S3, d'un manque d'espace ou d'un retard de protection supérieur au seuil configuré (5 minutes par défaut).
