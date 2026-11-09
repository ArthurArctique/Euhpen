"""Petite tête de classification posée sur l'encodeur gelé de Whisper.

Whisper n'écrit ni les « euh » ni les syllabes étirées (« leeee… », « donc-euuuh »). Ce paquet
ajoute un classifieur par trame de 20 ms (parole, hésitation, allongement, silence) entraîné sur
les états de l'encodeur, dont les détections s'insèrent dans la transcription sous forme de jetons
``[euh]`` et ``[étirement]``.

Rien ici n'est chargé tant que la fonction n'est pas demandée : PyTorch reste optionnel.

Licences : la tête est entraînée sur le corpus Rhapsodie (CC BY-NC-SA 3.0 FR). Ses poids, fournis
dans ``euhpen/weights``, héritent de cette licence : à créditer, sans usage commercial.
"""
