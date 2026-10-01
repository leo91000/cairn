# Réserver l'espace des écritures sans sérialiser leur I/O

Date : 2026-10-01.

Statut : implémentation en validation ; mesures locales, pas encore déployée.

## Problème

L'admission partageait un verrou entre tous les disques d'une node et les
remplissages de cache. Elle le conservait jusqu'à la fin de l'append durable.
Une synchronisation de segment faisait donc attendre les autres conversations.
Les compteurs internes au journal ne montraient pas cette attente en amont.

Sur la vraie VM, un essai de transport asynchrone expose quatre requêtes à
l'entrée du volume mais une seule au journal. Retirer simplement le verrou
ouvrirait une course sur la réserve physique et le quota partagé.

## Décision

Conserver l'admission sérialisée pour vérifier l'espace disponible, réserver
une marge conservatrice et débiter le quota mémoire. Relâcher ensuite ce verrou
avant l'append. Chaque écriture garde sa réservation jusqu'à la fin de son I/O,
y compris en cas d'erreur. L'append est toujours synchronisé avant sa réponse.
Le format du journal, le scellement et les reçus de publication restent identiques.

Le compteur des réservations en cours est borné par les requêtes admises, sans
file supplémentaire ni thread. Son addition vérifie les débordements ; sa garde
libère les octets à sa destruction, sans reprendre un verrou de maintenance.
La marge reprend celle de l'admission précédente : quatre fois la taille de la
requête, plus 1 Mio pour les métadonnées.

Toutes les sondes d'espace utilisent ce compteur, notamment les remplissages de
cache. Elles le lisent **avant** la consultation du système de fichiers et du
quota. Si une écriture finit pendant cette consultation, sa réservation reste
donc comptée conservativement dans cette réponse. Les admissions et remplissages
gardent leur verrou commun pendant ce calcul ; aucune nouvelle réservation ne
peut apparaître entre cette lecture et leur décision.

Le quota mémoire peut déjà compter une écriture en cours. Soustraire également sa
réservation peut temporairement surévaluer l'espace requis ; ce choix conserve
la protection quand une réconciliation du quota précède son allocation réelle.
Les sondes de santé n'admettent pas d'écritures et restent indicatives.

Le compteur est partagé par le contrôleur. Plusieurs chemins sur des systèmes de
fichiers distincts seraient traités conservativement ; le déploiement utilise
un répertoire d'état de stockage par contrôleur.

## Validation et limites

Les tests couvrent les réservations simultanées, leur libération après échec,
le débordement et l'impossibilité pour le cache d'utiliser l'espace réservé.
Ce dernier test utilise un processus isolé pour ne pas modifier les compteurs
globaux des autres tests. Les tests existants de stockage et de transitions du
contrôleur passent.

Le banc Firecracker prouve la suppression de la sérialisation avant le journal.
Il ne démontre pas à lui seul une réduction du temps de démarrage : le journal
continue à synchroniser chaque append. Le comparatif avec plusieurs VM, la charge
prolongée avec sauvegardes et la validation de l'image livrée restent nécessaires.
Les options FUSE/Firecracker asynchrones utilisées pour isoler le problème ne font
pas partie de cette décision.
