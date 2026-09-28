# Disques S3 obligatoires sur toutes les nodes

Le stockage à la demande devient le seul mode d’exécution des conversations isolées. Chaque nouvelle VM démarre avec un journal local durable et un manifeste de blocs publié sur S3. Le manager doit disposer de la configuration S3 ; chaque node d’exécution doit disposer de FUSE et pouvoir lire les blocs via le manager. Les limites de cache, la réserve de disque et la cadence de synchronisation restent configurables par node, mais il n’existe plus d’activation par node.

Le déplacement publie les dernières écritures de la source, clôt son droit d’exécution, puis monte le disque publié sur la destination. Une panne de la source avant publication permet seulement une reprise depuis le dernier état publié. La destination ne télécharge pas tout le disque avant de démarrer. La restauration complète vers une image locale et la migration automatique des anciennes images locales sont supprimées.

Chaque réservation démarre une VM avec le journal et le droit de lecture de sa conversation. Le préchauffage anonyme d’une image locale est supprimé. La reprise évite le téléchargement complet du disque, mais conserve le coût du démarrage de Firecracker et de l’agent de code.

L’agrandissement ext4 reste une opération exceptionnelle : les outils actuels nécessitent une image locale temporaire, ensuite reconvertie en journal avant tout démarrage. Il peut donc demander le téléchargement complet et suffisamment de disque libre. Cette limite ne s’applique ni aux reprises de même taille ni aux déplacements entre nodes.

Ce choix réduit les parcours de disque et évite qu’une node démarre silencieusement en mode local non synchronisé. Il impose S3 au manager et FUSE à chaque node avant une nouvelle exécution. Les anciennes conversations qui possèdent encore une image locale doivent être supprimées avant le déploiement de cette version ; leur disque n’est ni converti ni effacé automatiquement par le nouveau code. Le propriétaire a accepté cette suppression. Les écritures restent synchronisées de manière asynchrone selon l’ADR-0008.

Cette décision remplace l’activation par node et la migration progressive des ADR-0006 et ADR-0007, ainsi que le suivi dm-era des disques locaux de l’ADR-0005.
