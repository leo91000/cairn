> Décision révisée par [ADR-0008](0008-current-disk-publication-without-archives.md) : publication S3 courante, sans archivage ni historique de restauration.

# Préserver la reprise et les livrables partagés lors de l’archivage

L’archivage des conversations vise à libérer le stockage principal tout en permettant de reprendre le travail : il conserve donc la session agent et l’environnement de travail, en plus de l’historique et des fichiers. Ce choix augmente le volume archivé par rapport à un simple export de messages ; si une ancienne session native devient incompatible, une nouvelle session ne sera proposée qu’en conservant l’historique et les fichiers et en demandant l’accord de l’utilisateur.

Les livrables disposant d’un lien public doivent rester immédiatement accessibles même lorsque la conversation passe en stockage froid : leur disponibilité ne peut donc pas dépendre de la restauration de l’archive. Cette exception réduit les économies de stockage possibles, mais évite qu’un archivage automatique rende les liens partagés indisponibles pendant plusieurs heures. La mise en corbeille révoque ces liens immédiatement ; récupérer la conversation ne les republie pas.

Décisions confirmées pendant l’entretien de conception. Voir [le cycle de vie des conversations](../CONVERSATION-LIFECYCLE.md).
