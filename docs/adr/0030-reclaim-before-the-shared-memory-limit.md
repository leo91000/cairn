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

## Complément du 2026-10-04 : échec interne pendant la reprise

Le 2026-10-04 vers 19:03 UTC, deux exécutions de tâches ont été interrompues
à quatre secondes d’intervalle sur la node locale, après environ 2 min 30
sans activité. Leur reprise automatique a échoué en une à deux secondes avec
l’erreur interne générique du contrôleur. Le broker enregistrait alors le code
de sortie 1, et le manager affichait « Process exited with status 1 » comme un
échec définitif, alors que l’agent n’avait pas démarré.

Une erreur interne (500) d’une tentative est désormais enregistrée avec le code
`CONTROLLER_FAILED` (70). Le manager la reprend comme une interruption, mais
compte toujours ces reprises dans la limite de trois, y compris sur une node
distante : une erreur interne peut être déterministe. Au-delà, l’échec indique
que le contrôleur a échoué plutôt qu’un code de sortie de l’agent. Les autres
erreurs (validation, conflit, image invitée incompatible) gardent le code 1.

## Complément du 2026-10-05 : ballon de pression à double seuil

Le ballon de pression de l’ADR-0012 ne faisait que gonfler. Au-delà de 85 % du
budget, chaque guest en marche rendait jusqu’à ne garder que 256 Mio
disponibles, et rien ne lui rendait cette mémoire une fois la pression passée.
Une VM de 34 Gio est ainsi restée plusieurs heures avec environ 2 Gio
utilisables, sans cache de fichiers : une compilation Rust y a duré plus de
deux heures. Comprimer un guest à ce point déplace aussi ses lectures vers le
journal de l’hôte, dont le cache compte dans le même cgroup.

Le ballon reste piloté depuis la surveillance du pool, avec deux seuils.
Au-dessus de 85 %, un guest en marche rend au plus 256 Mio par tick, en gardant
1 Gio disponible. Sous 75 %, le ballon de pression se dégonfle de 256 Mio par
tick jusqu’à zéro. Entre les deux, sa taille ne change pas, pour éviter les
oscillations. Une VM en pause, ou dont le ballon est en train de gonfler pour
la rétention, n’est jamais dégonflée.
