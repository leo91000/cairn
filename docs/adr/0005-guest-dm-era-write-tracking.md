# Suivre les blocs écrits avec dm-era dans l'invité

> Décision historique remplacée par [ADR-0009](0009-s3-backed-disks-required-on-every-node.md). Les nouveaux disques utilisent le journal local du stockage S3.

Relire tout le disque d'une conversation à chaque point de reprise est irréaliste avec
des captures fréquentes. Firecracker ne suit que la mémoire, pas les écritures disque.
Le noyau invité, contrôlé par Leo, empile donc la cible dm-era sur le disque de données.
L'hôte ne copie ensuite que les blocs de 4 Mio signalés depuis le dernier point publié.
`data.ext4` reste une image ext4 brute : les métadonnées dm-era sont consultatives, et
les perdre ne coûte qu'une copie complète.

dm-thin avec `thin_delta` aurait permis de dégeler la VM avant la lecture. Mais il
change le format de stockage et impose de gérer le remplissage du pool. Surtout, ses
métadonnées deviennent indispensables pour lire le disque. Les options dans l'hôte
(dm-era hôte, vhost-user, ublk, NBD, FUSE) demandent des privilèges ou un service par
VM, ou sont encore en preview.

Toute écriture de l'hôte sur `data.ext4` invalide le suivi. Une copie complète est faite
au moins toutes les 24 h. Voir [la recherche et les mesures](../INCREMENTAL-VM-BACKUP-WRITE-TRACKING.md).
