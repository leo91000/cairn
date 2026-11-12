> Statut : Remplacée par [ADR-0008](0008-current-disk-publication-without-archives.md) pour les points de restauration et par [ADR-0083](0083-local-conversation-disks-without-s3.md) pour la sauvegarde distante ; la durabilité locale des écritures reste applicable.

# Disque local et sauvegarde incrémentale asynchrone

L’utilisateur souhaite protéger les VM avec un petit retard, sans retransférer
leur disque entier après chaque changement. Les écritures restent locales et les
changements sont sauvegardés en arrière-plan vers le master ou S3, avec des points
de reprise cohérents et un retard affiché. Ce choix accepte la perte des dernières
modifications non sauvegardées pour éviter de faire dépendre l’acquittement des
écritures de la latence et de la disponibilité du stockage distant.

Le format et la capture doivent permettre de réutiliser les données déjà reçues
et de préserver toutes les dépendances des points conservés. Ce choix ne promet
ni délai chiffré avant mesure, ni cohérence applicative sans coordination, ni copie
de la RAM.
