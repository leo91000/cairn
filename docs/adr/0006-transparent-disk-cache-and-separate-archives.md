> Décision révisée par [ADR-0008](0008-current-disk-publication-without-archives.md) pour l’archivage et par [ADR-0009](0009-s3-backed-disks-required-on-every-node.md) pour l’activation et la migration.

# Cache disque transparent et archivage distinct

Pour libérer le disque sans imposer une restauration complète avant chaque reprise, la copie locale de l'environnement devient un cache dont l'éviction laisse la conversation active et reprenable à la demande.
L'archivage reste une action distincte pouvant conduire au stockage froid, afin de conserver cette possibilité sans en imposer la latence aux conversations actives.
L'activation se fait par node avec migration progressive des environnements arrêtés et vérification de la copie distante avant libération locale.
