# Préchauffage borné et récupération des images runtime

Date : 2026-10-05.

## Constat

Une image CLI-update peut hériter d'un home Codex rempli. Le préchauffage
anonyme le refuse et recommence toutes les dix secondes. Les images runtime
locales, et leurs annonces côté manager, ne sont jamais récupérées.

## Décision

Une image CLI-update vide le home Codex après les opérations de mise à jour.
Seule une VM neuve et anonyme peut remplacer ce home hérité avant de recevoir
les schémas sans compte. Une VM déjà initialisée reste refusée.

Les erreurs de préchauffage attendent 10, 20, 40, 80 puis 160 secondes ; le
sixième échec ouvre le circuit pendant une heure. Le délai exponentiel est
plafonné à cinq minutes. Un succès remet le compteur à zéro ; un nouveau
runtime, chargé par un nouveau contrôleur, repart sans historique d'erreurs.
Les annulations ne comptent pas comme erreurs. Le refus de capacité des anciens
guests continue à désactiver le préchauffage pour ce contrôleur.

Au démarrage et toutes les dix minutes, le contrôleur récupère les images qui
ne sont ni courantes ni référencées par un disque, un environnement ou un
modèle. La publication des références et la récupération se sérialisent ; une
référence illisible fait abandonner le cycle entier. Les images sont incluses
dans le budget de disque alloué. Les annonces de runtimes sont des leases de
24 heures renouvelées lors des annonces ; les anciennes annonces permanentes
reçoivent aussi une échéance lors de la migration.

## Conséquences

Un préchauffage défectueux cesse de produire des écritures continues tout en
restant retestable. Une reprise conserve son image épinglée. Une image ancienne
sans référence peut être retéléchargée si un environnement distant la demande
avant l'expiration de son annonce ; après expiration, elle doit être annoncée
à nouveau par le manager du runtime concerné.
