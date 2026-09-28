# Déplacer une conversation par pause et reprise

Les nodes de Leo peuvent être disponibles de manière intermittente. Le déplacement
d’une conversation conserve son environnement de travail complet et sa session en passant par une pause
puis une reprise sur la destination ; il ne conserve pas les processus ni leur
mémoire, notamment GPU. Ce compromis, confirmé pendant l’entretien de conception,
accepte de devoir relancer les commandes interrompues pour éviter d’exiger la
migration à chaud des processus et du matériel.
