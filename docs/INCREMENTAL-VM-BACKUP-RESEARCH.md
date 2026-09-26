# Sauvegarde incrémentale des VM vers le master ou S3

Recherche du 26 septembre 2026, sans implémentation ni essai KVM/S3. L’utilisateur a confirmé en Q24 l’incrémental asynchrone avec un petit retard acceptable. Le mécanisme de capture reste à valider ; cette note ne garantit aucun délai de sauvegarde.

## Réponse

Oui : garder le disque actif local et transférer uniquement ses modifications est faisable. Il faut toutefois un mécanisme de capture des changements, des points de reprise cohérents et un format de sauvegarde incrémental. Une réplication asynchrone peut réduire la perte potentielle ; elle ne garantit pas que chaque écriture locale soit déjà sauvegardée à distance. C'est une conclusion d'architecture fondée sur les mécanismes ci-dessous, pas une capacité actuellement livrée par Leo.

## Point de départ de Leo et Firecracker

Leo utilise Firecracker 1.17.0 ([Dockerfile](../Dockerfile)), une image `root.ext4` en lecture seule et un `data.ext4` mutable initialement dimensionné à 32 Gio ([création et configuration](../backend/src/microvm/host.rs)). La migration convenue conserve l'environnement et la session, sans RAM ni processus ([ADR](adr/0002-conversation-movement-by-pause-and-resume.md)). L'image immuable peut donc être conservée par version ; le disque mutable et les autres données persistantes nécessaires doivent être inventoriés ensemble.

Les snapshots différentiels Firecracker suivent les **pages mémoire**, pas les modifications du disque. Les fichiers des disques restent à sauvegarder par l'intégrateur. Firecracker exige une pause pour son snapshot et draine/synchronise les écritures des disques lors de sa création. Une pause seule ne démontre donc pas que tous les caches sont vidés. [Documentation 1.17.0](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/docs/snapshotting/snapshot-support.md)

Le chemin le moins intrusif conserve les fichiers raw locaux avec les moteurs bloc standards. Un stockage externe via `vhost-user` existe, mais reste en developer preview dans 1.17.0 ; les snapshots Firecracker sont incompatibles avec ces périphériques. Ce n'est pas nécessaire pour une sauvegarde incrémentale côté hôte. [Moteurs bloc](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/docs/api_requests/block-io-engine.md), [vhost-user](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/docs/api_requests/block-vhost-user.md)

## Ce que S3 apporte

S3 ne fournit pas un disque avec réécriture aléatoire de secteurs dans un objet. L'append existe pour S3 Express One Zone dans les directory buckets, seulement en fin d'objet : cela ne résout pas les réécritures dispersées d'une image VM. [PutObject](https://docs.aws.amazon.com/AmazonS3/latest/API/API_PutObject.html), [Append](https://docs.aws.amazon.com/AmazonS3/latest/userguide/directory-buckets-objects-append.html)

Une représentation adaptée contient des fragments ou deltas immuables et un manifeste décrivant une sauvegarde complète. Proposition : uploader et vérifier toutes ses dépendances avant de publier le manifeste ; mettre à jour le pointeur courant conditionnellement. AWS garantit l'atomicité par clé, sans transaction multiclés ; le protocole Leo doit donc empêcher de présenter une génération incomplète comme restaurable. Vérifier ces propriétés séparément pour chaque fournisseur compatible S3. [Cohérence S3](https://docs.aws.amazon.com/AmazonS3/latest/userguide/Welcome.html#ConsistencyModel), [Écritures conditionnelles](https://docs.aws.amazon.com/AmazonS3/latest/userguide/conditional-writes.html)

## Approches réutilisables

| Approche | Transfert | Contraintes |
| --- | --- | --- |
| Snapshot stable puis sauvegarde dédupliquée, par exemple restic | Uniquement les fragments absents | Une image raw modifiée doit généralement être relue entièrement ; moins d'upload ne signifie pas moins de lectures locales. |
| Snapshots COW de l'hôte et export incrémental, par exemple Btrfs send | Changements entre snapshots immuables | Stockage hôte adapté, parents conservés, chaîne de restauration maîtrisée. |
| Journalisation des écritures bloc avec export continu | Écritures ordonnées regroupées en segments | Travail nettement plus profond : flush, concurrence, pannes, rejeu, troncature et bornes de reprise. |

Restic documente la relecture du contenu pour la déduplication quand le fichier a changé : cette première approche ne promet pas de suivre les blocs sans scan. Btrfs send exploite les différences entre snapshots et les références d'extents, évitant de devoir inventer un comparateur qui rehache toute l'image ; il impose des snapshots strictement en lecture seule et des références cohérentes à la restauration. [Restic](https://restic.readthedocs.io/en/stable/040_backup.html#file-change-detection), [Btrfs send](https://btrfs.readthedocs.io/en/latest/btrfs-send.html), [Format de flux](https://btrfs.readthedocs.io/en/latest/dev/dev-send-stream.html)

La journalisation est techniquement possible : `dm-log-writes` conserve les données et traite explicitement l'ordre WRITE/FLUSH, mais vise les tests de systèmes de fichiers, pas une sauvegarde de production prête à intégrer. Drafter fournit un précédent de migration assistée par S3, avec fork Firecracker et NBD ; son dépôt est archivé depuis octobre 2025. Une migration assistée ne prouve pas une restauration autonome et cohérente après perte de la source. [Linux](https://docs.kernel.org/admin-guide/device-mapper/log-writes.html), [Drafter](https://github.com/loopholelabs/drafter)

## Cohérence et délai réel

Copier des portions du disque pendant qu'elles changent peut produire un mélange d'instants. Il faut lire une vue stable ou rejouer un journal jusqu'à une borne validée. Un état cohérent après panne permet le redémarrage avec récupération des journaux ; une sauvegarde cohérente pour une application peut aussi nécessiter qu'elle vide ses caches ou termine une transaction. Les données encore uniquement en RAM ne sont pas protégées par une réplication disque. [Préparation des applications et gel des systèmes de fichiers](https://docs.cloud.google.com/compute/docs/disks/creating-linux-application-consistent-pd-snapshots)

En asynchrone, l'âge du dernier point restaurable dépend du débit sortant, du volume modifié et des incidents. Garantir la protection distante des écritures déclarées durables exige de faire attendre leur acquittement distant au bon niveau de flush/fsync : le réseau entre alors dans le chemin critique. Cela ne protège toujours pas les données que l'application n'a pas écrites. Cette conséquence découle du protocole d'acquittement, sans promesse de performance mesurée.

## Recommandation pour Leo

Viser des **sauvegardes incrémentales fréquentes et cohérentes**, asynchrones vers le master ou S3, avec affichage du dernier point réellement restaurable. Évaluer d'abord les snapshots hôte et outils existants ; choisir la fréquence après mesure, sans développer immédiatement un moteur bloc distribué.

Prévoir une base initiale, la vérification des fragments, une restauration complète sur une autre node, et une rétention qui conserve toutes les dépendances des trois points retenus. Trois points restaurables ne signifient pas trois deltas indépendants. Garder des points historiques protège aussi contre une mauvaise modification rapidement répliquée.

Avant validation : mesurer pause, lectures, transfert et restauration ; couper réseau et alimentation pendant capture/publication ; tester données Docker, session agent et bases locales. Vérifier fencing de l'ancienne node, clés de déchiffrement disponibles sur la destination et absence d'exposition des credentials. L'incrémental répond au coût d'upload ; le « temps réel » strict serait une exigence supplémentaire à décider.
