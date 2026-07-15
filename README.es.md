# TakeFive — Recordatorios tranquilos para jornadas más saludables

> Un compañero de escritorio local-first para quienes pasan muchas horas frente a una pantalla. Define ritmos para beber agua, descansar la vista, moverte, concentrarte y terminar el día; TakeFive se encarga del tiempo en segundo plano, sin hacer ruido.

<p align="center">
  <a href="README.md">English</a> ·
  <a href="README.zh-CN.md">简体中文</a> ·
  <a href="README.ja.md">日本語</a> ·
  <a href="README.es.md"><strong>Español</strong></a>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/version-0.1.0-E15D44" alt="Versión 0.1.0" />
  <img src="https://img.shields.io/badge/status-MVP%20preview-E7B755" alt="MVP de prueba" />
  <img src="https://img.shields.io/badge/platform-Windows-0078D4" alt="Windows" />
  <img src="https://img.shields.io/badge/Tauri-2-24C8DB?logo=tauri&logoColor=white" alt="Tauri 2" />
  <img src="https://img.shields.io/badge/license-MIT-2D7263" alt="Licencia MIT" />
</p>

<p align="center">
  <img src="docs/assets/takefive-reminder-surface.png" alt="TakeFive muestra un recordatorio suave en la esquina inferior derecha" width="520" />
</p>
<p align="center"><sub>Un aviso pequeño, justo cuando ayuda. No roba el foco, se cierra tras 7 segundos por defecto y permite completar, posponer, omitir o cerrar.</sub></p>

> [!IMPORTANT]
> TakeFive es ahora una versión MVP `0.1.0`. El flujo principal está implementado y cubierto por pruebas automáticas, pero el instalador de Windows firmado, el empaquetado de release y la validación completa en equipos reales siguen en preparación. No sustituye recordatorios médicos, de medicación ni de seguridad. macOS está planificado, todavía no publicado.

## La idea

La mayoría de las apps de recordatorios te obligan a administrar el recordatorio en vez de tu día. TakeFive propone algo más sereno: configura una regla una vez, mira cuándo ocurrirá la próxima, y sigue trabajando sin una tormenta de notificaciones.

### Pequeños hábitos para desarrolladores

| Situación | Lo que puedes hacer hoy | Dirección que estamos explorando |
| --- | --- | --- |
| **Ritmo para descansar la vista** | Crear un recordatorio alineado cada 45 o 60 minutos y asociarlo a una pausa breve. | Un preset 20-20-20 con superficies de descanso semitransparentes o a pantalla completa. |
| **Levantarse, estirar, hidratarse** | Programar avisos recurrentes para agua, movimiento o un hábito propio; pausarlos durante una reunión. | Tarjetas de ejercicios de 30 segundos, 1 minuto y 3 minutos. |
| **Límite para dormir** | Añadir un recordatorio único para empezar a cerrar la jornada. | Cuenta atrás para dormir y modo “esta noche no programes hasta tarde”. |
| **Control de cafeína** | Mantener todos los datos en local y crear un recordatorio de control. | Registro local de café, té y bebidas energéticas con una estimación del impacto en el sueño. |
| **Pomodoro con cuidado** | Combinar intervalos, horas tranquilas y una sesión de descanso iniciada por ti. | Sugerencias de agua, movimiento o un minuto lejos de la pantalla al terminar. |
| **Autochequeos ligeros** | Enviar avisos como “mira lejos durante 20 segundos” sin interrumpir el trabajo. | Tarjetas opcionales de autocuidado visual, sin diagnóstico médico. |
| **Revisión de la jornada** | Guardar eventos y resultados del planificador en local. | Un resumen semanal de concentración, noches largas, pausas omitidas e hidratación. |
| **Compañero de escritorio silencioso** | Funcionar desde la bandeja, sin conexión, incluso con la ventana principal cerrada. | Respiración guiada offline, ruido blanco y tarjetas de cierre del día. |

La columna derecha es una dirección de producto, no una lista de funciones ya publicadas.

## Por qué se queda encendido

- **Suave por defecto.** La superficie flotante inferior derecha no roba el foco, se cierra tras 7 segundos por defecto y ofrece completar, posponer, omitir o cerrar.
- **Fiable en segundo plano.** El planificador Rust mantiene el tiempo oficial; no depende de una pestaña del navegador ni de `setTimeout` en el front end.
- **Consciente de la recuperación.** Tras iniciar, despertar, desbloquear o cambiar la hora o zona horaria, reconstruye el estado desde SQLite en vez de reproducir avisos antiguos en masa.
- **Privado por diseño.** Sin cuenta, nube, telemetría, contenido del teclado, títulos de ventanas, capturas de pantalla, micrófono, cámara ni rastreo del ratón.
- **Reglas comprensibles.** Cada regla activa muestra su próxima ejecución y explica pausas, horas tranquilas y eventos que no se mostraron.

## Funciones actuales

| Área | Incluido en el MVP |
| --- | --- |
| Reglas | Horas fijas, intervalos alineados a un ancla, avisos únicos, días laborables/todos los días, ventanas activas y exclusión del almuerzo |
| Gestión | Crear, ver resumen y próxima ejecución, activar, desactivar y borrar de forma suave |
| Entrega | Superficie transparente inferior derecha, cierre configurable (7 segundos por defecto), completar/posponer/omitir/cerrar, cola de avisos y alternativa de notificación del sistema |
| Segundo plano | Bandeja del sistema, instancia única, planificación tras cerrar la ventana y arranque automático de Windows (activo por defecto, configurable) |
| Pausa y calma | Pausar todos los avisos 30 minutos, 1 hora o 2 horas; horas tranquilas diarias 12:00–13:30, también durante la noche |
| Idiomas | Interfaz en inglés, chino simplificado, japonés y español |

## Inicio rápido

```powershell
cd apps/desktop
npm ci
npm run tauri dev
```

Al cerrar la ventana principal, TakeFive permanece en la bandeja. Para terminarlo por completo, usa el menú de la bandeja.

Para requisitos, compilación, controles de calidad, arquitectura, privacidad y hoja de ruta, consulta el [README en inglés](README.md).

## Licencia

Publicado bajo la [licencia MIT](LICENSE).
