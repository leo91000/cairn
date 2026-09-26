# Conserver Firecracker et différer le GPU

L’accès GPU attendu doit être directement visible dans la VM de l’agent. Face à
l’absence de support GPU/VFIO publié dans Firecracker 1.17.0, l’utilisateur choisit
de conserver Firecracker et de différer le GPU plutôt que de déléguer les calculs
à un environnement externe ou de changer de moteur de VM. Le partage d’une même
carte entre plusieurs conversations reste un objectif futur distinct, dont la
faisabilité devra être vérifiée même si Firecracker ajoute un accès GPU direct.

Les nodes CPU, le déplacement complet des environnements et la reprise depuis
sauvegarde restent dans le périmètre. Voir les sources et décisions dans
[la conception des nodes](../DISTRIBUTED-NODES-DESIGN.md).
