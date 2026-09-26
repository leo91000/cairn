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
de la RAM. Voir [la recherche](../INCREMENTAL-VM-BACKUP-RESEARCH.md) et
[la synthèse](../DISTRIBUTED-NODES-SPEC.md).
