> Statut : Remplacée par [ADR-0084](0084-local-conversation-disks-without-s3.md) : changer de node fait expirer l’environnement de travail, qui n’est plus conservé par le déplacement.

# Déplacer une conversation par pause et reprise

Les nodes de Leo peuvent être disponibles de manière intermittente. Le déplacement
d’une conversation conserve son environnement de travail complet et sa session en passant par une pause
puis une reprise sur la destination ; il ne conserve pas les processus ni leur
mémoire, notamment GPU. Ce compromis, confirmé pendant l’entretien de conception,
accepte de devoir relancer les commandes interrompues pour éviter d’exiger la
migration à chaud des processus et du matériel.
