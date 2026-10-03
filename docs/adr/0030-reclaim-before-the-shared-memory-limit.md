# Récupération avant la limite de RAM partagée, et reprise des VM tuées

Date : 2026-10-03.

## Constat

Le 2026-10-03, la node de production a subi des OOM du cgroup partagé
`leo-shared` (`CONSTRAINT_MEMCG`) à 11:39, 11:47 et 13:13 UTC. Les VM n’en
étaient pas la cause : au dernier incident, la mémoire anonyme ne représentait
que 5,5 Go, contre 31,5 Go de cache de fichiers actif (dont 1,6 Go sale) sur
une limite de 37,2 Go. L’allocation déclenchante venait du thread du journal
ublk (`GFP_NOFS | __GFP_NOFAIL`), qui ne peut pas récupérer ce cache. Le noyau
a tué des processus Firecracker, puis le processus `leo` du contrôleur, ce qui
a redémarré le runner et arrêté toutes les VM.

Les conversations ont repris seules, mais les exécutions de tâches ont échoué :
le broker enregistrait le code de sortie 1 pour toute erreur, y compris
« Guest disconnected », alors que le manager ne reprend une tentative que sur
`CONTROLLER_INTERRUPTED`.

## Décision

Le contrôleur fixe `memory.high` un dixième sous `memory.max` à chaque
application du budget. Au-dessus de ce seuil, le noyau récupère le cache et
ralentit les allocations au lieu de tuer un processus. `memory.max` reste la
limite physique de l’ADR-0012. Pendant l’incident, ce réglage posé à la main
a ramené l’usage de 38,1 à 31,6 Go sans pression mesurable (PSI `some` à
0,06 % sur 5 minutes).

Une erreur « indisponible » d’une tentative (guest déconnecté, VM tuée) est
enregistrée comme `CONTROLLER_INTERRUPTED`, comme une lease perdue. Le
manager reprend alors l’exécution depuis son disque conservé, dans la limite
existante de trois reprises du contrôleur, sans plafond sur une node
distante. Les erreurs de validation et les incompatibilités d’image invitée
(protocole, disques appariés) ne sont pas « indisponibles » et restent des
échecs définitifs, pour ne pas boucler sur une node distante.

## Conséquences

Les pics proches de la limite coûtent de la latence au lieu d’une perte
d’exécution. Sans swap, la mémoire anonyme d’un guest au-delà du seuil n’est
pas récupérable : ce guest, et le contrôleur du même cgroup, sont ralentis
tant qu’elle y reste. Le ballon commence déjà à 85 % du budget.

Le cache du journal n’est pas vidé explicitement après les synchronisations :
il est relu par les VM, et la récupération par `memory.high` suffit à le
contenir. Cette option reste à réévaluer si la pression mémoire du cgroup
devient durable.
